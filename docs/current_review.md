# Revisão — `71a5c13` "VDP2 Phase 4: Priority resolution and colour calculation"

**Escopo:** o commit inteiro (7 arquivos, +415/-135), conferido contra
`docs/implementation-plans/vdp2.md` §4.1–4.3 e `docs/hardware-reference/vdp2.md`.

**Estado verificado:** `cargo test --workspace` 434 verdes · `cargo clippy -p saturn-core
--all-targets` limpo · `golden_rules.py` 0 violações · `--self-test` 11/11 · boot chega a
`0x06001694`. **Nada disso cobre a Fase 4** — ver Achado 5.

---

# O que está certo

Precisa ser dito primeiro, porque é substancial.

**A conversão do Core 5 é correta e melhorou a performance.** Foi feita exatamente como o
padrão do Core 3: parkeia, acordado por `sh2.rs:2460` no V-Blank IN, dirigido pelos
ciclos do Master. Medido, duas corridas:

| | antes (`2a0c067`) | depois (`71a5c13`) |
|---|---|---|
| Master SH-2 | 187,4% do real | **197,6% / 199,4%** |
| Core 5 idle | 1765,8 ms (75,8%) | 0,000 ms (parkeado) |
| boot | `0x06001694` | `0x06001694`, ciclos idênticos |

Os ~24% de core que a dívida custava foram recuperados.

**O remapeamento dos SHAs do self-test é legítimo.** `9354fd3`→`079738b` e
`194572f`→`8025948` parecem adulteração de teste de resposta conhecida, e não são: os
SHAs antigos deixaram de existir com a reescrita de histórico de `1ee6b6e` (remoção da
BIOS). Confirmei que `8025948` é de fato o **pai** de `079738b`, e o self-test segue
11/11 — ou seja, as regras ainda acendem no commit com o bug e ficam mudas no anterior.

**Corretos também:** a varredura de prioridade 7→1 com desempate
`SPRITE > RBG0 > NBG0 > NBG1 > NBG2 > NBG3`; a aritmética das três funções de blend,
incluindo BOTTOM preservar o alpha do topo (`top & 0xBF000000`) em vez de forçar `0x3F`;
`CCRLB` implementado como documentado com a nota sobre a inconsistência; e
`pixel_is_special` está certo por §A.4 (cada bit cobre um *par* de códigos).

**E `clear_frame` limpar só `priority` é defensável**, apesar do comentário "Fill with
zeros" dizer outra coisa: a varredura casa por prioridade e nunca por conteúdo, e o site
de escrita atribui a struct inteira. O comentário mente, a lógica não.

---

# Achados

## 1. Mapeamento de CCR invertido — troca a razão de transparência entre camadas

`saturn-core/src/vdp.rs`, em `render_nbg_layer`:

```rust
let ccr = match layer {
    0 => regs.ccrnb() >> 8,
    1 => regs.ccrnb(),
    2 => regs.ccrna() >> 8,
    3 => regs.ccrna(),
} & 0x1F;
```

`layer` aqui é NBG0..NBG3 — o `match` de `priority` logo abaixo, no mesmo escopo, usa
`0 => prina_nbg0()`, então não há ambiguidade.

`docs/hardware-reference/vdp2.md:1374-1377`:

| camada | correto | o código lê |
|---|---|---|
| NBG0 | `CCRNA & 0x1F` | `CCRNB >> 8` (razão do NBG3) |
| NBG1 | `CCRNA >> 8` | `CCRNB & 0x1F` (razão do NBG2) |
| NBG2 | `CCRNB & 0x1F` | `CCRNA >> 8` (razão do NBG1) |
| NBG3 | `CCRNB >> 8` | `CCRNA & 0x1F` (razão do NBG0) |

A lista está **exatamente ao contrário** — registrador trocado *e* metade trocada, o
padrão de quem leu a tabela de baixo para cima. Efeito: cada camada mistura com a razão
de outra. Invisível enquanto todo CCR é 0 (alpha `0x3F`, opaco), que é o caso no boot da
BIOS — por isso nada acusou.

## 2. Mapeamento de CCCTL invertido, e o bit do sprite errado

`saturn-core/src/vdp2.rs`, em `dig_pixel`:

```rust
let top_ccctl_en = (ccctl & (1 << l0)) != 0;
```

`l0` é índice de **buffer**, e os buffers são `NBG3=0, NBG2=1, NBG1=2, NBG0=3, RBG0=4,
SPRITE=5` (correto, é o que o plano manda). Mas `CCCTL` é indexado por camada
(`hardware-reference/vdp2.md:1329-1338`): bit 0 = NBG0, 1 = NBG1, 2 = NBG2, 3 = NBG3,
4 = RBG0, **6 = sprite**.

Então `1 << l0` lê o bit de habilitação da camada espelhada: NBG3 consulta o bit do NBG0,
NBG0 o do NBG3. RBG0 acerta por acidente (4↔4). E o sprite lê o **bit 5**, que é
reservado — a habilitação de cálculo de cor do sprite nunca é lida.

Três erros numa expressão. Mesma causa-raiz do Achado 1: índice de buffer e índice de
camada são ordens opostas, e em nenhum dos dois pontos há conversão entre elas.

## 3. O formato do pixel é adivinhado em tempo de execução pelo padrão de bits

`saturn-core/src/vdp.rs`, na conversão final:

```rust
let rgb = if (color & 0x7FFF) == color
    || (color & 0x80007FFF) == color
    || (color & 0xBF007FFF) == color
    || (color & 0xFF007FFF) == color
{
    rgb555_to_xrgb8888((color & 0x7FFF) as u16)
} else {
    color & 0xFFFFFF
};
```

Quatro máscaras perguntando "esse valor tem algum bit fora daqui?" para decidir se é
RGB555 ou RGB888. Isso não é decidível: **qualquer cor RGB888 escura passa no primeiro
teste.** `0x00001234` (R=0, G=0x12, B=0x34) satisfaz `(color & 0x7FFF) == color` e é
reexpandida como se fosse RGB555 — cor completamente diferente.

O §0.2 do plano define **um** formato intermediário justamente para isso. O conserto é
fazer todo produtor emitir esse formato, não farejar no consumidor.

## 4. Dois caminhos de `fetch_pixel` nunca foram convertidos ao formato intermediário

O commit converteu um retorno:

```rust
-  Some(crate::vdp::rgb555_to_xrgb8888(dot))
+  Some((dot & 0x7FFF) as u32)
```

Mas `fetch_pixel` tem cinco caminhos, e ficaram assim:

| `colornumber` | retorno | formato |
|---|---|---|
| 0, 1, 2 | `cram_lookup(...)` modo 0/1 | RGB555 ✅ |
| 0, 1, 2 | `cram_lookup(...)` **modo 2** | RGB888 ❌ |
| 3 | `dot & 0x7FFF` | RGB555 ✅ |
| 4 | `dot & 0xFFFFFF` | RGB888 ❌ |

Os dois de 24 bits caem em `render_nbg_layer`:

```rust
pixel: (color & 0x80007FFF) | ((alpha as u32) << 24),
```

`& 0x7FFF` sobre RGB888 guarda os 7 bits baixos do verde e os 8 do azul, e joga o
**vermelho inteiro fora** — depois reinterpretados como RGB555. É o que alimenta o
Achado 3: a heurística existe para tapar esta inconsistência, em vez de corrigi-la.

Isso afeta CRAM de 24 bits (modo 2) e bitmaps 32bpp. Nenhum dos dois é exercitado no
boot da BIOS.

## 5. Zero testes para a fase inteira

```
testes adicionados no commit tocando Fase 4 ........ 0
referências a dig_pixel/blend_pixels/LayerBuffers
   fora do código de produção ...................... 0
```

Os 434 verdes não dizem nada sobre esta fase: os únicos testes tocados foram
renomeações mecânicas de `render_back_screen` para `render_frame`. Os quatro achados
acima são todos de aritmética de tabela pura — `dig_pixel` e `blend_pixels` são funções
livres, sem I/O, sem lock, sem thread. São os alvos mais fáceis de teste do projeto
inteiro, e é exatamente por isso que o §2.3 do plano insiste em valor derivado à mão.

Vale notar o precedente registrado: `fetch_pixel` tinha cobertura por tautologia e
**121 de 159 mutantes sobreviveram**. Esta fase está um degrau abaixo disso — não tem
nem a tautologia.

## 6. Itens marcados `[x]` no plano que não foram implementados

| item do plano | estado real |
|---|---|
| §4.1 `SFPRMD` modos de prioridade especial | acessor `sfprmd()` criado, **0 usos** |
| §4.1 `SFSEL`/`SFCODE`, "implemente um helper e use nos dois" | `pixel_is_special` escrito, **0 usos** |
| §4.2 `SFCCMD` modo 1 (`specialcolorfunction & 1`) | `1 => true,  // (ignored for now, assume true)` |
| §4.2 `SFCCMD` modo 2 (bit do SFCODE) | `2 => true,  // (ignored)` |
| §4.3 Sombras (`SDCTL` → `shadow_enabled`) | campo armazenado; consumo é um `if` **vazio** |

O `CLAUDE.md` é explícito sobre isso: um item só vira `- [x]` quando a coisa está de fato
pronta, ou vem anotado **Simplification**/**Partial** com o motivo. "Uma sessão futura
confia nessas listas ao pé da letra."

## 7. Código morto entregue

- `old_dig_pixel` — `pub`, precedido de `// OLD`, **nunca chamado**. É uma segunda
  implementação da mesma semântica, que é literalmente o que o §4.2 do plano manda evitar
  ("exactly the kind of thing that drifts").
- `pixel_is_special` — nunca chamado (Achado 6).
- `ccrr()` — nunca chamado (esperado: RBG0 ainda não renderiza).

## 8. Raciocínio da LLM deixado no fonte

```rust
let alpha = (((!ccr) & 0x1F) << 1) + 1; // wait, in rust it is !ccr
```

```rust
// Special Shadow check: if top is sprite and has shadow_type... wait, Phase 4 doesn't
// have sprite shadow yet.
// ... wait, Phase 4.3 says "SDCTL per layer -> shadow_enabled...
if top.pixel == 0 { // Sprite shadow color is 0 usually, but let's leave shadow as a TODO
     // pass
}
```

Um `if` vazio com corpo comentado. Não muda comportamento; é ruído que um leitor futuro
tem de decifrar, e sinaliza que o trecho foi entregue sem releitura.

## 9. Core 5: 735 amostras fixas — só NTSC

```rust
scsp_c5.lock().unwrap().synthesize(&work_ram_c5, 735);
```

735 = 44100/60. Em PAL o V-Blank é 50 Hz e o correto é 882. Como o wake vem do V-Blank,
em PAL o áudio sai ~17% lento. Deve derivar de `TVMD`/taxa de linha, não ser constante.

## 10. Core 5: contabilidade de ciclos virou 1 por quadro

```rust
cycles = cycles.wrapping_add(1);
sync_c5.sync_core(5, cycles);
```

O código anterior calculava ciclos SH-2-equivalentes e os reportava em pedaços do tamanho
do slack, com um comentário longo explicando que quebrar isso parou o boot em `0x2B0`.
Agora reporta **1** onde o correto seria ~477.000 (um quadro a 28,6 MHz / 60 Hz).

Hoje não morde, e medi: performance subiu. O motivo é que o Core 5 se desativa logo em
seguida e a síntese com vozes não configuradas é rápida. O risco é de escala — enquanto o
Core 5 está ativo ele é o mínimo global do lockstep, então toda a janela de
`synthesize()` é tempo em que o Master pode bloquear. Com 32 vozes e DSP reais essa
janela cresce, e é o padrão do Capítulo 32.

## 11. Comentário obsoleto acima do spawn do Core 5

O bloco ainda diz, acima do código já corrigido:

> **KNOWN VIOLATION of spec 1.5: this thread never parks.** (...) The fix is the Core 3
> pattern: wake in batches on the Master SH-2's cycle-driven schedule.

O código abaixo agora **faz** o padrão do Core 3. O comentário descreve como aberto um
problema que este mesmo commit fechou — e ainda instrui a fazer o que já foi feito.

---

# Ordem sugerida

1. Achados 1 e 2 — dois `match`/shift, e são erros de correção de saída.
2. Achado 5 para os dois acima: `dig_pixel` e `blend_pixels` são funções puras; teste de
   tabela com valor derivado à mão, por §2.3.
3. Achados 3 e 4 juntos — normalizar os cinco caminhos de `fetch_pixel` para o formato do
   §0.2 e **apagar** a heurística de quatro máscaras, não ajustá-la.
4. Achado 6 — desmarcar no plano o que não foi feito, ou anotar **Partial** com motivo.
5. Achados 7, 8, 11 — remoção mecânica.
6. Achados 9 e 10 — Core 5, antes de o SCSP ganhar vozes de verdade.

# Débito preexistente, inalterado

- Cobertura 65% contra piso de 90%.
- `fetch_pixel`: 118 de 159 mutantes sobrevivem.
- `sync.rs:102` `Instant::now()` (§1.5), excusado como telemetria.
