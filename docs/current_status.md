# Estado atual: performance medida e as duas desconfianças

Medido em 2026-09-20, `cargo build --release`, nesta máquina: **AMD Ryzen 5 3500X
(6 núcleos)** e **GeForce RTX 3060**. Este arquivo tem duas partes bem
diferentes: a primeira é medição, a segunda é **palpite declarado**. Não
confunda uma com a outra.

---

# 1. Performance (medido)

## Em tempo real, com vídeo e som na janela

O binário `live`, com o limitador de quadro ligado, rodou a BIOS a
**59,5 fps — 0,99× tempo real**, que é o alvo (59,94 Hz).

| | valor |
|---|---|
| CPU do processo | **~80% de um núcleo** (pico 89%) |
| CPU da máquina inteira | ~21% dos 6 núcleos |
| GPU | **~20–22%** de utilização |
| Controlador de memória da GPU | ~12% |
| Consumo da GPU | 17,3 W (42 W no arranque) |

Os três primeiros segundos ficam em 52% de CPU e a GPU sobe de 3% a 22%: é o JIT
ainda compilando blocos enquanto o driver gráfico sobe. Do quarto segundo em
diante estabiliza em ~80%.

## Sem limitador: quanto a máquina aguenta

| o que roda | ms/quadro | fps | × tempo real |
|---|---|---|---|
| CPU só (sem GL, sem som) | 7,67 | 130 | 2,17× |
| CPU + som | 9,18 | 109 | 1,82× |
| CPU + som + GL na janela | 11,3 | 88,6 | 1,48× |

Orçamento de quadro a 59,94 Hz é **16,68 ms**; gastamos **11,3 ms**. Sobra cerca
de um terço.

- **Som custa 1,5 ms/quadro (16%).** Medido em 1800 quadros, não em 600: o DSP de
  efeitos só sobe por volta de 10 s de boot, e em 600 quadros a conta sai barata
  demais.
- **GL custa 2,2 ms/quadro (19%).**
- Por segundo, o `live` sem limitador variou entre **65 e 123 fps**; o mínimo é o
  primeiro segundo, com o JIT compilando.

## Onde o tempo vai (perfil, 1800 quadros, DSP ativo)

| | |
|---|---|
| `Saturn::step` (orquestração) | **25,4%** |
| JIT SH-2 `run_block` | 23,7% |
| som (68000 + SCSP + DSP de efeitos) | 16,9% |
| barramento e espaço de endereço | ~11% |
| temporização de vídeo | 2,4% |

O perfil e o relógio concordam no som: 16,9% no perfil, 16,4% medido desligando
o som. Quando as duas contas batem, dá para confiar nas outras linhas.

---

# 2. Otimização de orquestração

**Atualização (2026-09-20): os dois primeiros caminhos abaixo foram feitos e
medidos.** `advance()` em `src/machine.rs` agora preserva a capacidade do vetor
de eventos (drena em vez de consumir) e faz um único empréstimo do SMPC por
passo em vez de três. Medido com `mimasv2 --frames 1800` (CPU + som, sem GL),
três rodadas de cada lado, binário `--release`:

| | antes | depois |
|---|---|---|
| tempo de parede | 17,62 s (média de 3) | 16,55 s (média de 3) |
| `Saturn::step` no `perf` (auto-amostragem, 999 Hz) | 25,16% | 24,02% |

**6,1% mais rápido**, e o `perf` concorda: caiu, não só o tempo de parede. Não
é o "só o perfil mudou de nome" que a seção original avisava para desconfiar —
os dois lados moveram juntos. `bash tools/quality_gate.sh` continua 9/9 verde
depois da mudança (vídeo e trace inalterados: é otimização, não comportamento
diferente). Dois testes novos em `src/machine.rs` (`advancing_past_a_video_event_does_not_reset_the_event_queues_capacity`,
`a_full_frame_of_stepping_does_not_panic`) cobrem o caminho.

**Atualização (mesma sessão): o item 3 (serviço por prazo) também foi feito, só
que só para o CD block — não para DMA/interrupção, por motivo explicado abaixo.
O item 4 (blocos maiores no JIT) foi tentado e revertido: mediu, não ajudou.**

- **Item 3, versão restrita — `cd.tick()` em lote.** O único efeito visível do
  relógio do CD block é `HIRQ_SCDQ`, que sobe uma vez a cada 381.800 ciclos
  (~1/75 s). Chamá-lo a cada bloco do JIT — muitas vezes sob cem ciclos — é um
  empréstimo de `RefCell` e uma chamada para nenhuma mudança observável na
  maioria das vezes. Agora os ciclos se acumulam em `cd_cycle_carry` e só são
  entregues ao CD block quando passam de `CD_TICK_BATCH_CYCLES` (1024, menos de
  uma linha de varredura — três ordens de grandeza abaixo do período que
  alimenta). Teste novo (`the_cd_clock_still_advances_despite_batching`) prova
  que o lote não perde ciclos: avança em passos de 200, bem menores que o lote,
  até passar do período do SCDQ, e confirma que o bit ainda sobe.

  **DMA e entrega de interrupção ficaram como estavam, de propósito.** Ao
  contrário do CD block, os dois são sensíveis a atraso: interrupção só é
  aceita entre blocos do JIT (invariante documentada), então adiar a entrega
  encolheria ainda mais uma janela que já é a mais larga que existe; e DMA na nossa
  emulação é instantâneo (sem estados de espera de barramento modelados), então
  um laço de espera do driver conta com ver o efeito logo depois do evento que
  o disparou. Nenhum dos dois aparecia no `perf` como custo relevante — a
  chamada em si é barata quando não há nada pronto — então adiá-los trocaria
  risco de regressão de trace por um ganho que a medição não mostrou existir.

- **Item 4 — blocos maiores no JIT, tentado e revertido.** Dobrei
  `MAX_BLOCK_INSNS` de 64 para 128 e medi: trace idêntico (92,1%, mesmo drift),
  vídeo idêntico (1,66/255), e tempo de parede **sem diferença mensurável**
  (16,64/16,62/16,55 s contra 16,55/16,59/16,50 s antes — dentro do ruído).
  Revertido para 64. Explicação provável: o código da BIOS desvia com
  frequência, então a maioria dos blocos já termina bem antes do teto atual —
  dobrar um teto que quase nunca é atingido não muda o tamanho médio do bloco.
  Fica registrado porque é exatamente o tipo de "parece que devia ajudar" que
  só a medição desmente — e a regra do projeto é reportar isso, não escondê-lo.

Medido de novo depois de tudo isso (CPU + som, sem GL, `--frames 1800`, média
de 5 rodadas): **16,09 s**, contra 17,62 s antes de qualquer mudança desta
seção — **8,7% mais rápido no total**, com vídeo e trace bit a bit iguais ao
que eram antes de mexer em qualquer coisa aqui.

O texto abaixo é a análise original, mantida como registro do raciocínio; os
itens 1, 2 e 3 (na sua versão restrita) da lista não são mais pendência, e o
item 4 foi tentado e descartado por medição, não por suposição.

## JIT no 68000 (som): ligado, medido, mantido (2026-09-20)

O 68000 é a única outra CPU de verdade que executa código de propósito geral
neste emulador — o SH-1 do CD block não roda nada (é stub comportamental) e o
`scu_dsp` executa um programa de 32 palavras uma vez no boot, não um fluxo
contínuo. O `SoundCpu::advance` soma com o resto do `m68k::core::*` cerca de
**13% do tempo de parede** no `perf` — candidato real, e o único.

O crate `m68k` tem um recurso `jit` (Cranelift), mas ele **só se liga através
de `run_batch`** — verifiquei no fonte (`core/execute.rs`): o `trace_jit` só é
chamado dentro de `run_batch`/`run_batch_inner`; `run_for_cycles`, que era o
que usávamos, nunca passa por ali. E `run_batch` **abre mão da contagem de
ciclos** (a própria doc do crate diz: "`cycles_remaining` is clobbered";
`BatchResult` só devolve instruções) — e a taxa de amostragem do SCSP depende
exatamente do número de ciclos que `SoundCpu::advance` devolve
(`Scsp::generate`, `M68K_CYCLES_PER_SAMPLE`). Não dava para trocar sem
reconciliar isso.

**Primeiro um microbenchmark**, fora da emulação, rodando o driver real da
BIOS (`--dump-sound-ram`) pelos dois caminhos, orçamento grande (favorece o
JIT o máximo possível):

```
run_for_cycles: 5.736.411 instruções em 226 ms (25,4 Minsn/s)
run_batch (jit): 5.555.555 instruções em 174 ms (31,9 Minsn/s)
```

26% mais throughput de instrução — sem `FastMem`/`TrackedMem` (a RAM de som
vive atrás de `Rc<RefCell<SoundRam>>` compartilhado com a SH-2; expor um
ponteiro bruto para dentro disso durante o `run_batch` seria o tipo de
`unsafe` que arrisca UB por aliasagem, não tentei). Por essa conta isolada,
26% de um pedaço que já é 13% do total dava uma estimativa pessimista de
3–4% no tempo de parede total — parecia pouco para o risco de reescrever a
régua de tempo do som.

**Liguei mesmo assim e medi o sistema inteiro, porque o microbenchmark usa
orçamentos grandes e a integração real usa orçamentos de ~64 ciclos (~7
instruções) por chamada — o cenário real é bem pior para o JIT do que o
benchmark isolado, e só medir o sistema inteiro diz a verdade.** Troquei
`run_for_cycles`/`run_for_cycles_with_hook` por `run_batch` em
`SoundCpu::advance` (`src/devices/sound_cpu.rs`), convertendo o orçamento em
ciclos para um orçamento em instruções via uma média medida (`AVG_CYCLES_PER_INSTR`,
2.000.000 ciclos / 222.897 instruções — a mesma medição de `docs/sound.md`,
não um palpite novo) e reconstruindo o número de ciclos "gastos" a partir de
quantas instruções o lote realmente rodou. O perfil (`--sound-profile`, que
precisa de um PC por instrução) não tem variante com esse gancho em
`run_batch`, então fica no caminho antigo, exato — é diagnóstico, não o
caminho quente.

**O risco real não era velocidade, era a taxa de amostragem do áudio
depender de uma estimativa em vez de um número exato.** É exatamente o tipo
de regressão que passaria batido num teste funcional e apareceria só como
"o som está sutilmente errado" — e é exatamente o que o passo 9 do gate
(`compare_audio`, piso 0,707, sem folga acima do valor medido) existe para
pegar. Depois da troca: **gate 9/9 verde, correlação de áudio 0,707 —
idêntica**, vídeo e trace inalterados, 79 testes passando. Se a aproximação
tivesse deslocado a cadência do som o suficiente para importar, esse piso
sem folga teria caído.

**Tempo de parede, medido A/B intercalado** (o mesmo binário-JIT e o
binário-intérprete rodados alternados, 6 pares, para cancelar ruído do
sistema — a comparação sequencial simples tinha dado números inconsistentes
por causa de builds e testes rodando entre uma medição e outra):

| rodada | JIT | intérprete |
|---|---|---|
| 1–6 | 16,84 / 16,86 / 16,74 / 16,73 / 16,82 / 16,74 s | 17,11 / 17,01 / 16,99 / 17,05 / 17,10 / 17,01 s |
| média | **16,79 s** | 17,05 s |

**JIT ganhou nas 6 de 6 rodadas** — pequeno (~1,5%), mas consistente, não
ruído. Menor que os itens 1–3 de orquestração, maior que zero, e sem folga
perdida no piso de áudio. Mantido. `m68k = { version = "0.14.0", features =
["jit"] }` no `Cargo.toml`.

Caminho que aumentaria o ganho, se algum dia vier a valer o risco:
`TrackedMem` para a RAM de som (leitura direta, escrita ainda passando pelo
barramento). Não tentei — a aliasagem compartilhada com a SH-2 torna o
`unsafe` arriscado demais para medir "de brincadeira", e o ganho de 26% já
medido no microbenchmark isolado é o teto otimista, não o que a integração
real entregaria.

**A orquestração custa mais que a emulação.** `Saturn::step` tem quatro linhas;
os 25,4% são o `advance` inlinado nela, e ele roda **uma vez por bloco do JIT**.
Por bloco — que muitas vezes tem poucas instruções — fazemos:

- `self.cd.borrow_mut().tick(cycles)` — um `RefCell` e um tick do CD block;
- `self.timing.advance(...)`;
- `std::mem::take(&mut self.events)` — o vetor sai e volta **sem capacidade**, e
  o próximo `push` realoca;
- `self.smpc.borrow()` e depois `self.smpc.borrow_mut()` — dois empréstimos onde
  cabia um;
- `sound_step`, que faz mais dois `borrow_mut` (RAM de som e SCSP);
- `service_vdp1`, `run_dmas`, `deliver_interrupt`.

São dezenas de milhões de repetições de uma taxa fixa. **A desconfiança é que
quase nada ali precisa acontecer por bloco.** Quatro caminhos, do mais barato ao
mais invasivo:

1. **Preservar a capacidade do vetor de eventos.** `if !self.events.is_empty()`
   em volta, e devolver o vetor depois do `drain`, em vez de `mem::take` seco.
   Uma linha, e tira uma alocação por quadro do caminho quente.
2. **Um empréstimo por chip por passo**, em vez de vários. O SMPC hoje leva dois
   seguidos para ler uma flag e limpá-la.
3. **Serviço por prazo, não por bloco.** O CD block, o DMA e a entrega de
   interrupção não mudam de resposta dentro de um bloco de 64 instruções.
   Acumular ciclos e só chamar quando cruzar o próximo evento de vídeo (ou um
   prazo curto) substitui dezenas de milhões de chamadas por algumas milhares.
4. **Blocos maiores no JIT.** O teto atual é 64 instruções e a orquestração é
   cobrada por bloco; bloco maior dilui a taxa fixa. Mexe em invariante do JIT,
   então é o último a tentar, e só com o gate medindo antes e depois.

**Como saber se acertei:** o perfil precisa mostrar `Saturn::step` caindo *e* o
tempo de parede caindo junto. Se só o perfil mudar, o custo mudou de nome, não
de lugar.

Dois avisos honestos:

- **O ganho tem teto.** Se os 25,4% virassem 5%, iríamos de 1,48× para cerca de
  1,9× tempo real. É folga, não é outra ordem de grandeza.
- **Não é o gargalo que o usuário sente hoje.** Já rodamos em tempo real com
  folga de um terço. Isto é preparação para jogo, não para a BIOS.

E o que **não** é gargalo: a GPU, em 20% de um 3060 para desenhar 320×224. E não
somos limitados por núcleo: usamos um, e cinco estão ociosos.

---

# 3. Erro de som: o que era desconfiança agora está resolvido em parte (2026-09-20)

Sintoma relatado originalmente: **tonalidade e velocidade estão certas, mas o som
fica em loop, sem cauda e sem o "shuuuan"**.

**Atualização:** um envelope por tempo (não o das quatro fases do hardware —
ver `docs/sound.md`, seção "2026-09-20: um envelope, mas não o do hardware") já
está implementado em `src/devices/scsp.rs`. Medido com `--dump-audio` em 620
quadros: o pico cai de 2214 (0,8 s) para 36 (2,0 s) e para um piso quase
inaudível por volta de 2,6 s, em vez de tocar cheio até 8,5 s. O loop e a falta
de cauda, como sintomas, estão corrigidos. O texto abaixo é a análise que levou
até ali; fica como registro do raciocínio, não como trabalho pendente na parte
do loop/cauda.

## Desconfiança principal: não existe gerador de envelope

Temos **TL estático** onde o hardware tem quatro fases — ataque, decaimento 1,
decaimento 2 e liberação — com as taxas derivadas de KRS, OCT e FNS. Três
consequências, e elas explicam os três sintomas na ordem em que foram relatados:

1. **O loop.** Um slot com LPCTL ligado repete a região de loop para sempre. No
   hardware quem o cala é o envelope, ao chegar no fundo da atenuação — não um
   key-off. Sem envelope, ele repete até o fim dos tempos. Foi exatamente isso
   que a medição mostrou: o driver **não toca nos registradores do slot entre
   0,634 s e 8,779 s**. O loop é o que ele mandou; quem devia encerrá-lo não
   existe aqui.
2. **A falta de cauda.** A cauda é a fase de liberação: o volume descendo ao
   longo de centenas de milissegundos. Com volume constante não há descida, e
   portanto não há cauda — o som simplesmente continua e depois corta.
3. **A falta do "shuuuan".** Esta é a parte com mais palpite e menos certeza. O
   DSP de efeitos já confere bit a bit com a referência, então o reverb está
   sendo calculado certo. O que desconfio é que o "shuuuan" **é o reverb ficando
   audível quando a nota some** — com a nota em volume cheio o tempo inteiro, o
   retorno do reverb fica mascarado por ela e nunca aparece como evento separado.
   Se for isso, o envelope resolve os três sintomas de uma vez.

## Desconfiança secundária: pode faltar uma segunda nota

Está medido, na nossa própria emulação (não na referência), que **há uma única
chave de slot em todo o boot** (`1 key-ons`). Como o nosso driver de 68000 é o
programa real da BIOS rodando sobre o crate `m68k` — não uma reimplementação —
esse número já é uma medição do que o programa real manda fazer, não uma
suposição. Se a máquina real ligasse um segundo slot, teria de ser por um
caminho que este driver não percorre, o que é pouco provável mas não
impossível de descartar sem instrumentar a referência.

**Atualização mais forte (2026-09-20, depois de obter PCM real de referência):**
`stubs/captures/audio/boot.wav` — 12 s gravados por loopback do YabaSanshiro
instrumentado, não uma suposição — mostra **dois picos**, não um: o primeiro
(5,75–8,0 s, pico ~3455) é a nota; depois de uma baixa, um segundo pico **mais
alto que o primeiro** (8,5–9,75 s, pico 7781), e só então a queda limpa até o
silêncio por volta de 11,75 s. Uma segunda nota de slot apareceria como um
evento independente, não como esse formato de "nota, baixa, pico maior,
decaimento junto" — a forma é exatamente o que "reverb atrasado e mascarado"
prevê. Isso não é mais leitura do nosso próprio despejo (que é o que a versão
anterior deste parágrafo comparava) — é a gravação real. Ainda não é prova
definitiva (seria preciso a captura por amostra do envelope para separar as
duas hipóteses sem ambiguidade), mas é evidência bem mais forte do que a
anterior, e aponta na mesma direção.

## O que ficou faltando, e por quê

O envelope implementado é **por tempo, não por registrador** — ver
`docs/sound.md`. Agora existe um piso mensurável para ele: `compare_audio`
contra `stubs/captures/audio/boot.wav` dá correlação 0,707 (passo 9 do gate), e
a gravação mostra que a forma real tem duas corcovas, não um decaimento
monótono — o que o relógio fixo atual não reproduz. Isso já é o suficiente para
guiar ajuste do formato (quanto tempo segurar, quanto tempo decair) e medir a
cada tentativa. O que continua faltando é mais fino: decodificar AR/D1R/D2R/RR/KRS
dos registradores 0x08/0x0A do slot exigiria um oráculo por amostra (curva de
envelope real, não só o PCM final) que não existe. Sem ele, qualquer layout de
bits seria palpite travestido de fato — o mesmo erro que já custou caro nesta
sessão (o `envKey` do Qwen, confiado por documentação em vez de medido). Fica
como próximo item
em `docs/sound.md`, com o mesmo padrão de captura que já validou o DSP de
efeitos: dado, não código.

## O que já está certo, e não deve ser mexido ao perseguir isto

- O **DSP de efeitos** confere bit a bit com a referência: 99,7% de amostras
  idênticas em 60.000, pico igual, correlação 1,000000, **0 passos divergentes
  em 108** no trace passo a passo. Se o som continuar errado, o erro não está
  aqui.
- A **cadeia de mixagem** é a do hardware: seco por DISDL, envio por IMXL/ISEL,
  retorno por EFSDL/EFPAN, mestre por MVOL. O pico da saída caiu de 32767
  (saturando) para 7528.
- **Tonalidade e velocidade estão certas** — o próprio relato confirma, e isso
  fecha com passo, OCT/FNS e o relógio do 68000 estarem corretos.

Ou seja: o que falta é o **contorno do volume no tempo**, e nada mais do caminho
de sinal. É trabalho conhecido, não descoberta.

---

# 4. JIT SH-2: backend separado do despacho, para um segundo backend não custar do zero (2026-09-20)

Pergunta que veio de fora: o JIT roda em ARM64? A resposta curta é não — o
`m68k` (som) sim, via Cranelift, mas o nosso próprio compilador SH-2
(`dynasmrt::x64::Assembler`) é x86-64 só, e trocar de arquitetura significa
escrever um segundo backend, não virar uma flag. Isso é esperado de qualquer
emulador com JIT (cada arquitetura de host precisa do seu), mas o pedido foi
específico: organizar para reaproveitar o máximo de código quando esse segundo
backend existir.

**O que já era verdade, sem eu ter desenhado assim:** `src/cpu/jit/mod.rs` (o
cache de blocos e o despacho) nunca tocava em nada específico de x86-64 — só
chamava `Compiler::compile(...)` e tratava `CompiledBlock` como opaco
(`buf: ExecutableBuffer`, `entry: AssemblyOffset`, ambos tipos já
arquitetura-agnósticos no próprio `dynasmrt`, não só `dynasmrt::x64`). Isso
significa que o padrão de estratégia já existia na prática; só não estava
declarado como tal.

**O que mudou:** `src/cpu/jit/compiler.rs` virou `src/cpu/jit/backend/x64.rs`,
e `src/cpu/jit/backend/mod.rs` (novo) seleciona o backend por
`cfg(target_arch)` e reexporta `CompiledBlock`/`Compiler`/`MAX_BLOCK_INSNS`. É
seleção em tempo de compilação, não um objeto de trait: só um backend é
compilado por vez, então um `Box<dyn _>` custaria despacho dinâmico no caminho
mais quente do emulador por uma escolha que já está fixa no build. Um alvo que
não seja x86-64 falha a compilação com `compile_error!` explicando a interface
que um novo backend precisa implementar — falhar cedo, não silenciosamente
rodar sem JIT (a regra 2 do projeto é sem interpretador).

**O que não foi extraído, e por quê — isso é o achado real, não o refactor em
si.** O laço que decide onde um bloco termina (decodifica, conta custo,
verifica `MAX_BLOCK_INSNS`) está interligado com a emissão de código, não
separado dela. Tentei separar e parei ao ler `emit_branch`: o alvo do desvio é
calculado e guardado num registrador **antes** de emitir o delay slot, porque
o delay slot roda com o estado de registrador de antes do desvio e pode
sobrescrever o próprio registrador de onde o alvo foi lido — isso é semântica
do SH-2, não sintaxe de x86-64. Um "planeje o bloco, depois emita" compartilhado
precisaria carregar essa ordem como dado, não só a lista de instruções, e
errar isso é o tipo de bug que o trace-check não pegaria de cara (muda tempo
de execução sutilmente, não o PC visitado). Documentado como comentário em
`backend/mod.rs` e em `emit_branch`, para quem escrever o segundo backend não
precisar redescobrir isso do zero.

Gate depois da mudança: 9/9 verde, 0 linhas novas de cobertura pendente (é
reorganização de arquivo — só comentários mudaram de conteúdo, `diff_coverage.py`
não encontrou nada executável para medir).

---

# 5. SCSP para um jogo que "sintetiza tudo": Tiers 0–4 (2026-09-20)

Capturamos 10 minutos de Magic Knight Rayearth via YabaSanshiro instrumentado
(seção anterior, e `docs/sound.md`). Achado que motiva tudo isto: o jogo
escreve nos registradores do SCSP em média **215 vezes por quadro**, sem
parar — a BIOS mal toca o SCSP depois do primeiro segundo (1 key-on). Passei
um menu de 5 opções (JIT, SIMD, cache, threading), o usuário escolheu
**Tiers 1–4 juntos, Tier 5 (mais JIT no 68000 via TrackedMem) só se ainda
precisarmos espremer mais depois**.

## Feito e verificado nesta sessão (Tiers 0, 1, 2a, 2c)

Ordem real de implementação: 0 → 1 → 2a → 2c. Gate 9/9 verde (2 pulados, ver
Tier 0) depois de cada um, testes bit-exatos preservados o tempo todo.

- **Tier 0 — comparação de jogo no gate.** `stubs/captures-game/frames/frame<N>.png`
  (259 quadros migrados da captura do MKR) e `stubs/captures-game/audio.wav`
  (ainda não existe) são os caminhos padronizados — um jogo por vez, sem nome
  de jogo no caminho. Passos 10/11 do gate (`tools/quality_gate.sh`) leem
  desses lugares e **pulam** (não falham) enquanto faltar referência ou
  enquanto o mimasv2 não souber rodar um jogo de verdade (CD Block é stub).
  `docs/quality-gate.md` documenta os dois passos.
- **Tier 1 — `Scsp::diag_enabled`.** `common_writes`/`key_log`/`slot0_log`
  (só usados pelo relatório de `--sound-profile`) agora ficam atrás de uma
  flag, padrão `false`. Zero custo fora do modo diagnóstico.
- **Tier 2a — valores derivados de registrador cacheados em `Slot`**
  (`attenuation`, `pitch_step`, `dry_shift`, `dry_pan`, `send_shift`, `isel`),
  calculados uma vez no key-on e recalculados em `after_write` só quando o
  registrador dono muda — em vez de recomputados a cada amostra (44100×/s),
  incluindo o `powf` de TL que antes rodava sempre.
- **Tier 2c — cache de conteúdo para efeitos de um tiro.** Voz sem loop
  (`loop_mode == 0`) que termina de tocar guarda a saída "núcleo" (RAM +
  TL + envelope, antes de pan/envio) cacheada por
  `(SA, LSA, LEA, pitch, TL, pcm8)`; a próxima vez que a mesma chave tocar,
  reproduz do cache em vez de tocar RAM ou rodar o envelope de novo.
  Teste `a_repeated_one_shot_replays_from_cache_instead_of_rereading_ram`
  prova que é cache de verdade, não coincidência: muda a RAM entre as duas
  execuções e confirma que a segunda ainda bate com a primeira.
  Música em loop **fica de fora de propósito**: o envelope declarado hoje só
  decai, nunca sustenta, então não existe estado periódico não-silencioso
  para cachear ainda — isso espera o gerador de envelope de verdade (já listado
  em `docs/sound.md`). A chave de cache já está no formato certo para essa
  extensão quando ele existir.

**O que não dá para medir ainda:** o gate de hoje só exercita a carga do boot
da BIOS (1 key-on). O ganho de verdade destes quatro itens é sob a carga
sustentada de um jogo — só se mede depois que o driver do MKR rodar dentro do
nosso próprio emulador, que é trabalho declarado fora de escopo aqui.

## Ainda por fazer: Tiers 2b, 4 e 3 — desenho já validado, guardado aqui

O usuário pediu para parar a implementação aqui e só guardar o resto do plano
em docs, para retomar depois sem perder o desenho já resolvido.

### Tier 2b — SIMD no laço de mixagem, com escopo honesto

Sem `std::simd`/nightly no projeto (edition 2024, stable). Dois passos, nessa
ordem:

1. **Primeiro corte: reestruturar para autovetorização.** Separar a
   aritmética sem branch de dado (atenuação, pan por shift, clamp) num laço
   apertado sobre um buffer pequeno, sem `continue` cedo, para o LLVM
   vetorizar sozinho — zero `unsafe` novo, zero `cfg(target_arch)` novo.
2. **Só se o passo 1 não bastar:** intrínsecos `std::arch::x86_64` explícitos,
   atrás de `is_x86_feature_detected!("avx2")` com fallback escalar
   obrigatório. Escopo deliberado: só atenuação/pan/clamp entram em SIMD; a
   busca de amostra na RAM (gather, endereço depende de estado por slot) e o
   `mixs[isel] +=` (scatter-add de 32 fontes em até 16 destinos) **ficam
   escalares** — dizer isso explicitamente, não é omissão.

Oráculo: os testes bit-exatos de `scsp.rs` (`key_on_plays_the_sample_at_unity_pitch`
→ `vec![128, 256, 384, 512]` e companhia) não podem mudar por reassociação de
ponto flutuante.

### Tier 4 — JIT do microcódigo do DSP de efeitos

Arquivo: `src/devices/scsp_dsp.rs`. `Op` (26 campos), `ops: [Op; 128]`,
`run_sample` chama `exec(step, ram)` num laço reto `for step in 0..last_step`,
`exec` (~170 linhas) branch pesado sobre os campos de `Op`.

**Por que é seguro e o do 68000 não era:** o DSP roda exatamente uma vez por
amostra, sem acoplamento de ritmo/ciclo — deixar mais rápido não tem risco de
correção, só ganho de velocidade. `set_program` é raro (poucas vezes por
boot).

**Recomendação: especialização por closures, não um JIT `dynasm` de verdade,
como primeiro corte.** Os ~15 campos booleanos/pequenos de `Op` são
constantes de compilação para o programa carregado agora — construir, a cada
recompilação (disparada por um `dirty: bool`, checado uma vez no topo de
`run_sample`, **não** a cada uma das 512 escritas de um upload de programa),
uma lista de funções especializadas por passo remove os branches em runtime
sem gerar código de máquina, sem `unsafe` novo, e é bem menor que replicar o
padrão `dynasm` de `src/cpu/jit/backend/x64.rs` para ~15 campos × 128 passos.
Se depois de medir ainda sobrar custo relevante, um JIT `dynasm` de verdade é
o próximo passo natural.

Oráculo: `src/bin/dsp_check.rs` — 0 passos divergentes em 108, antes/depois,
mesma disciplina do experimento de bloco maior do JIT SH-2.

### Tier 3 — som (68000 + SCSP + DSP) em thread própria (o maior risco, de longe)

Desenho já especificado em `docs/sound.md` ("O desenho da thread de som"):
sincronização de mão única, RAM de som pertence à thread de som, escritas
chegam como mensagens carimbadas por ciclo numa fila SPSC, caixa de correio
sai de um retrato publicado, a SH-2 nunca espera o som.

**Estado atual confirmado:** `scsp_ram`/`scsp` são `Rc<RefCell<T>>` via
`Shared<T>` (`src/bus/device.rs:27`), registrados no barramento em
`Saturn::new`. `sound_cpu: SoundCpu` é dono direto em `Saturn`. `sound_step`/
`advance` (`src/machine.rs`) chamam tudo de forma síncrona, todo bloco do JIT.

**A questão de determinismo — decidida, não para adiar de novo quando isto
for retomado:** o gate inteiro depende do caminho headless produzir saída
idêntica byte a byte a cada execução. Decisão: implementar com uma **barreira
de dreno** — o caminho headless espera a thread de som drenar até o último
ciclo publicado pela SH-2 antes de ler `saturn.audio` ou despejar arquivo.
Isso preserva determinismo lógico (toda mensagem carimbada por ciclo, consumida
em ordem) sem exigir que o SO agende as duas threads de um jeito específico.
**Rede de segurança já aprovada:** se `dsp_check`/`compare_audio` não
reproduzirem bit a bit em 5–10 execuções seguidas mesmo com a barreira, cair
para: threading só como opt-in no `live`, caminho headless continua síncrono
como hoje.

**Caixa de correio:** sound RAM `0x700..0x73A` (handshake, `docs/sound.md`).
Retrato de tamanho fixo (`[u8; 0x40]`) atrás de um seqlock, atualizado uma vez
por lote drenado.

**Fila SPSC:** sem dependência disso hoje (`Cargo.toml`: 6 deps nomeadas).
Recomendação: fila própria à mão (~100 linhas), não puxar `ringbuf` — combina
com o apetite do projeto de possuir código de escopo apertado.

**Arquivos que mudam:** `src/devices/sound_thread.rs` (novo — spawna a
thread, mensagens, filas, seqlock, API de dreno); `src/devices/ram.rs` e
`src/devices/scsp.rs` (dono passa a ser a thread de som); `src/machine.rs`
(`Saturn::new` troca o tipo de porta nos dois `bus.map`, `sound_step`/
`advance` encolhem para "publicar ciclo"); `src/main.rs`,
`src/bin/dsp_check.rs`, `src/bin/compare_audio.rs`, `src/bin/live.rs` (chamar
a barreira de dreno antes de ler `saturn.audio`).

**Por que fica por último quando isto for retomado:** maior risco do plano
inteiro, e sua reescrita do caminho de escrita (`after_write` → mensagem
enfileirada) precisa carregar adiante a lógica já simplificada pelos Tiers 1 e
2a — não redesenhá-la sob threading ao mesmo tempo.

## Verificação, quando cada tier for retomado

- **2b:** testes bit-exatos de `scsp.rs`; `bash tools/quality_gate.sh`
  completo olhando o passo 9 (`compare_audio`); A/B intercalado de
  `mimasv2 --frames 1800`, mesmo método usado para o JIT do 68000.
- **4:** `dsp_check` antes/depois, 0/108 divergências exigido.
- **3:** `dsp_check`/`compare_audio` rodados 5–10 vezes seguidas exigindo
  saída idêntica — esse é o checkpoint de determinismo, uma rodada verde não
  basta. Para `live`: fps/CPU em tempo real, mesmo formato da tabela na
  seção 1 deste arquivo.
