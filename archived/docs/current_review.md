# Revisão — sessão do DeepSeek-V4.1-Flash (via Qwen Code)

**Escopo:** as mudanças não commitadas da sessão (`saturn-core/src/scu.rs`,
`cargo fmt` em `m68k.rs`/`sh2.rs`/`shared_buffers.rs`/`main.rs`) e o relatório
que ela escreveu neste arquivo, conferido afirmação por afirmação.

**Resumo:** o processo foi disciplinado — mediu antes e depois, reportou que a
porcentagem não se moveu, não tocou em `tools/`, não commitou, removeu o próprio
probe (`[COPYPROBE]`). O resultado de engenharia é **zero mudança de
comportamento**, uma correção reivindicada que não aconteceu, e uma conclusão
central errada, causada por um probe de debug que está vivo no `HEAD`. Os dados
brutos que ela coletou, porém, bastaram para provar a causa real do
descarrilamento do M68K (achado 5). O achado 6 desta revisão, que apontava
duas interrupções não atendidas, foi **retratado**: era um ponto cego do nosso
próprio instrumento.

---

## 1. A correção reivindicada não aconteceu

O relatório diz: *"Estava `lvl.write_address.wrapping_add(lvl.write_add)`; a
referência diz `write_add >> 1`. Corrigido."*

O `write_add >> 1` do ramo `src_is_bbus` está no código desde `0b8eb7d`
("milestone 2"). No diff contra o `HEAD` ele aparece como contexto, não como
adição. Saldo de código da sessão: **zero linhas**. O que entrou foi um comentário
e um teste.

## 2. O comentário contradiz o próprio teste

- Diz que o destino avança *"2 bytes for this 16-bit unit — i.e. `WriteAdd >> 1`"*.
  Isso só vale com `WriteAdd = 4`. O teste usa `WriteAdd = 2`, e o próprio teste
  diz que o passo é **1 byte**.
- Diz que usar o `WriteAdd` inteiro *"would stride the write past its own 16-bit
  footprint and leave every other destination word untouched"*. Com
  `WriteAdd = 2`, o passo inteiro é contíguo; é o meio passo que faz as escritas
  se sobreporem (o teste diz isso literalmente).
- *"the asymmetry is real, not an oversight in Yabause"* — afirmado sem evidência.

O comentário descreve o caso `WriteAdd = 4`; o teste exercita `WriteAdd = 2`.

## 3. O teste protege a linha, mas fixa um caso degenerado com procedência falsa

**O que vale:** sem o `>> 1` o teste falha; com ele passa. Ele guarda de verdade a
linha existente.

**O que não vale:**

- `DnAD = 0x101` nesse modo gera escritas de 16 bits em **endereço ímpar**,
  sobrepostas, perdendo metade dos dados (origem `11 22 … 88` vira destino
  `11 33 55 77 88`). O barramento do SH-2 não faz escrita de word desalinhada; o
  teste fixa como correto um comportamento que dificilmente é o do hardware.
- *"Fixture is a real one: the mirror of the BIOS sound-driver upload, SCSP sound
  RAM `0x05A00000` → High WRAM `0x06002000`"* — falso em três pontos: o teste usa
  `0x06000000`; o upload do driver de som da BIOS **não é DMA**, é o laço de
  escrita-e-verificação do SH-2 em `0x0600166E`; e o próprio relatório mostra que
  o DMA do boot vai para `0x25C20000` (VDP1).
- `let src_wram = 0x05A0_0000` é sound RAM, não WRAM.

**Recomendação:** manter um teste do passo `>> 1`, mas com `WriteAdd = 4` (o caso
contíguo, que faz sentido) e sem as afirmações de procedência.

## 4. A conclusão central está errada — o M68K "consertado" é o probe

O relatório diz que o M68K *"deixa de descarrilar após a correção"* e recomenda
reescrever a priorização porque *"o descarrilamento do M68K não era o que limitava
a cobertura"*.

Isso é impossível: nenhum código mudou, e o próprio Achado 3 do relatório mostra
que o boot nem passa pelo ramo `src_is_bbus`. O que aconteceu é o probe
`[M68K START]` em `M68k::step()`, **commitado no `ddf3d56`**: ele incrementa
`UNIMPL_LOG_COUNT` a cada instrução, estoura o limite de 20 antes do
descarrilamento, e o log de erro some. É a armadilha descrita no `QWEN.md` §5 —
que a apresentava como coisa do passado; o probe está vivo no `HEAD`, e isso é
falha da documentação, não só do modelo.

## 5. Os dados dela provam outra causa: o laço de limpeza do M68K se apaga

No primeiro opcode inválido:

```
opcode=0xFFFC at pc=0x00003232  d=[000000CF, …, 0000F373]  a=[00003230, …]
```

Entrada no laço (log `first entry to clear-loop`): `A0 = 0`, `D7 = 0xFFFF`,
`D0 = 0`.

```
0x322E: MOVE.L D0,(A0)+     ; zera 4 bytes em A0, A0 += 4
0x3230: DBF D7,-4
```

Depois de `0xC8C` voltas, `A0 = 0x322C` e o `MOVE.L` zera `0x322C–0x322F`,
**incluindo o próprio opcode em `0x322E`**. Na volta seguinte o M68K lê `0x0000`
= `ORI.B #imm,D0`, consome `0x51CF` (o opcode do `DBF`) como imediato, faz
`D0 |= 0xCF`, e cai em `0x3232` = `0xFFFC`.

Os três registradores batem exatamente: `D0 = 0xCF`, `A0 = 0xC8C × 4 = 0x3230`,
`D7 = 0xFFFF − 0xC8C = 0xF373`. **Não é corrida com o upload nem memória não
escrita** (essa foi a leitura do relatório, e antes a minha): o M68K apagou a
sound RAM a partir do zero — vetores de reset e o próprio código incluídos. No
reset os vetores estavam lá (`first16=[00 00 A0 00 00 00 10 00 …]`).

A pergunta real: **por que `A0 = 0` ao entrar em `0x322E`.** O código que
prepara `A0`/`D7` foi apagado da sound RAM pelo próprio laço; o original precisa
sair da cópia do driver (origem do upload em `0x06010000`), não do dump.

## 6. `0x06000846` é um trampolim de interrupção — é a pista, não um sintoma

> **Retratado em 2026-09-18, depois de estudar os traces ordenados do
> YabaSanshiro.** Os bytes decodificados abaixo estão certos — é mesmo o
> trampolim do V-Blank OUT —, mas a conclusão não: **nós atendemos essa
> interrupção.** O `MIMAS_PC_TRACE` não enxerga a primeira instrução depois que
> uma interrupção é aceita (o `step()` aceita e já executa essa instrução na
> mesma chamada; o gancho roda antes do `step()`), e nós executamos
> `0x06000848` e o dispatcher `0x060008F4` logo em seguida. A inferência de que
> `0x864` seria o Sound Request também cai: a máscara que a BIOS real escreve
> (`0xFFFFFF7D`) libera V-Blank OUT e SMPC, não Sound Request. A divergência real
> com disco é no frame 255, na caixa de correio do driver de som — ver
> `docs/unlock_bios/03-m68k-sound-driver.md`.
>
> **Nisso o DeepSeek estava certo**: a recomendação dele de não perseguir
> `0x06000846` procedia, e o parágrafo abaixo que a chama de errada, não.


O relatório conclui que a região é escrita durante a execução — **verdadeiro**,
mas a evidência citada era a *existência* do probe `[WATCH]`, não sua saída.
Rodando (boot com disco, 25 s):

```
[WATCH] write_high_ram_long to 844 = E0402F06
[WATCH] write_high_ram_long to 848 = A054E041
```

Decodificado:

```
0x844: E040   MOV #0x40,R0         ; delay slot do trampolim anterior (vetor 0x40)
0x846: 2F06   MOV.L R0,@-R15       ; entrada do vetor 0x41
0x848: A054   BRA 0x8F4            ; dispatcher comum
0x84A: E041   MOV #0x41,R0         ; delay slot: número do vetor
```

`0x846` é a entrada do **vetor 0x41 da SCU, V-Blank OUT**. Com trampolins de 6
bytes, `0x864 = 0x846 + 5 × 6` seria o **vetor 0x46, Sound Request** — isto é
inferência pelo espaçamento; só os bytes de `0x844–0x84B` foram observados.

Então a primeira lacuna são **duas interrupções que o hardware real atende e o
nosso não**. O Sound Request é levantado pelo driver do M68K, que descarrila
(achado 5) — cadeia coerente, ainda não medida. A recomendação do relatório de
*"não perseguir `0x06000846`"* está errada.

## 7. Imprecisões menores no relatório

- "primeiro byte não-zero em `0x3220` (12.848)" — 12.848 é `0x3230`.
- "~256 KB esperados" — é o tamanho do laço de limpeza (`D7 = 0xFFFF` × 4), não de
  um upload.
- "escreve … em passos de 2" — o laço de `0x0600166E` avança com `ADD #4,R3`.
- "Cobertura 65%" — estava em 77,84% na última medição.
- "Achados 1–11 da revisão anterior … nenhum foi resolvido" — vários foram, em
  `c759bf9` e antes.
- "(código do usuário, deixado no lugar)" — os probes são do Gemini.
- Rodou `cargo fmt` sobre os probes em vez de apontá-los, o que os faz parecer
  código intencional.

## 8. Não é da sessão, mas está no `HEAD` e afetou a medição

O `ddf3d56` commitou os quatro probes do Gemini:

| arquivo | probe | efeito |
|---|---|---|
| `m68k.rs` | `[M68K START]` em `step()` | queima `UNIMPL_LOG_COUNT`; cala o diagnóstico do M68K |
| `sh2.rs` | `[DEBUG] MISMATCH` no `run_loop` | checagem por instrução no caminho mais quente |
| `shared_buffers.rs` | 3× `[WATCH]` | comparação em toda escrita de High WRAM |
| `main.rs` | `std::fs::write("/tmp/mimas_ram.bin", …).unwrap()` | dump hardcoded — no commit cuja mensagem diz remover um |

Devem sair num commit de limpeza. O `[WATCH]` merece ser lembrado como a fonte do
achado 6 antes de ser removido.

---

## Próximo passo, na ordem

1. Remover os quatro probes do `HEAD`.
2. Descobrir por que `A0 = 0` na entrada do laço de limpeza do M68K — desmontar o
   driver a partir da cópia de origem, não do dump.
3. Verificar se V-Blank OUT (vetor 0x41) e Sound Request (0x46) são levantados, e
   se estão mascarados na SCU.

## Débito preexistente, inalterado

- Mapeamento de CCR invertido em `vdp.rs` (`render_nbg_layer`).
- Cobertura da árvore abaixo de 90%.
- `fetch_pixel`: 118 de 159 mutantes sobrevivem.
