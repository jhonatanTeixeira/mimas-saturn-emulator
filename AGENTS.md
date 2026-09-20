# AGENTS.md

**Arquivo único de instruções, para todos os agentes.** Claude Code, Qwen Code e
Antigravity descobrem `AGENTS.md` sozinhos; não existe `CLAUDE.md`, `GEMINI.md`
nem `QWEN.md` neste repositório, de propósito. Arquivo por ferramenta significa
três cópias que divergem, e um ponteiro ("leia o outro arquivo") é instrução
mole: só funciona se o agente resolver obedecer, e falha calado quando não
obedece.

Guia para agentes trabalhando neste repositório. Ele diz **como trabalhar
aqui**, e não o que está acontecendo agora: números, resultados e lacunas vivem
em `docs/status.md`, que muda a cada sessão. Onde um documento discordar de uma
medição, **a medição vence**.

## O que é isto

`mimasv2` é um emulador de Sega Saturn em Rust (edição 2024) com um objetivo
estreito: **bootar a BIOS real (`saturn_bios.bin`) sobre um JIT x86-64 e
reproduzir a saída de vídeo dela, sem tela**, quadro a quadro, contra as
capturas reais em `stubs/captures`.

**Código e comentários em inglês.** Vale para tudo que se escreve daqui em
diante, inclusive mensagens de log novas. O código anterior a 2026-09-19 está em
português e não precisa ser traduzido em massa; quando mexer num trecho, escreva
o novo em inglês.

## As regras do projeto

1. **Só vídeo, e o que o vídeo depende, é real.** Todo o resto é stub que
   devolve o mínimo para a sequência da BIOS seguir.
2. **Só JIT. Sem interpretador, e sem volta para interpretador por opcode.**
   Toda instrução SH-2 é compilada para x86-64 (dynasm). O interpretador antigo
   está em `legacy_src/` (fora do git e fora do build); nunca ressuscitar.
3. **Renderização é OpenGL puro e headless:** EGL surfaceless mais o crate `gl`
   cru. Nada de glium, winit ou glutin.
4. **Nunca leia `../yabassanshiro`, `../yabause` ou qualquer emulador vizinho.**
   Tudo sai dos traces, das capturas, do `saturn_bios.bin` e de conhecimento
   geral de SH-2/Saturn. Foi essa regra que fez a BIOS bootar em 3 horas depois
   de meses presos ao estilo do Yabause. A única exceção é capturar trace novo,
   que usa a **saída** de um emulador instrumentado, nunca o código dele:
   `tools/trace-capture/`.
5. **OOP e SOLID:** a CPU/JIT só conhece a trait `Sh2Bus`; chips implementam
   `MemoryDevice`; um chip novo entra registrando-se no barramento, sem tocar na
   CPU nem nos chips existentes.
6. **`archived/` está fora dos limites.** É o mimas antigo, de arquitetura
   diferente e abandonada. Não leia, não copie, não cite, não dependa. As
   permissões em `.claude/settings.json` bloqueiam a leitura, e
   `tools/project_rules.py` reprova qualquer referência a ele.
7. **Nunca commite por iniciativa própria — só quando pedirem.** Terminou um
   trabalho, deixe na árvore e reporte o que mudou. Quando pedirem, commite:
   commits pequenos, frase curta, sem título e sem linha em branco, **em
   inglês**, e sem marca d'água de ferramenta.

## Comandos

```bash
cargo build --release                                        # sempre --release para execuções reais
cargo test
./target/release/mimasv2 --frames 620 --dump out             # roda a BIOS, escreve out/frameN.png
./target/release/compare stubs/captures out --max-frame 728  # nota os quadros contra as capturas reais
./target/release/mimasv2 --frames 700 --trace-check bios_trace_no_game.txt
cargo run --release --bin live                               # janela: BIOS em tempo real, com vídeo e som
cargo run --release --bin gl_probe                           # sanidade: contexto EGL surfaceless + leitura do FBO
./target/release/mimasv2 --frames 300 --dump-sound-ram som.bin   # despeja a RAM de som (o driver que a BIOS carregou)
cargo run --release --bin m68k_probe -- som.bin              # roda esse driver no núcleo 68000 e mostra o que ele faz
DSP_STATE=x_dsp_state.txt DSP_RAM=x_dsp_ram.bin DSP_STEPS=x_dsp_steps.txt \
  ./target/release/dsp_check x_dsp_program.txt x_dsp_io.txt  # DSP de efeitos contra a captura real, passo a passo
./target/release/mimasv2 --frames 750 --dump-audio nosso.wav
./target/release/compare_audio stubs/captures/audio/boot.wav nosso.wav  # forma do envelope contra a captura real
bash tools/quality_gate.sh                                   # antes de dar qualquer trabalho por pronto
```

Flags de depuração do `mimasv2`: `-v` (stubs, DMA, DSP, SMPC, acessos não
mapeados), `--profile` (PCs de entrada dos blocos mais quentes — acha laço de
espera rápido), `--break <pc[,pc]> --break-n N --break-depth D` (as últimas D
instruções com todos os registradores na N-ésima batida; exige modo trace),
`--video` (registradores do VDP1/VDP2 e resumo da lista de comandos), `--vram
<hex,hex>`, `--dump-from/--dump-to/--dump-every`, `--sound-profile` (PCs mais
quentes do driver de som — acha o 68000 preso em laço de espera),
`--dump-audio`, `--dump-dsp`, `--dump-sound-ram`. `VDP_LAYER=0..4` renderiza uma
camada só (0-3 = NBG0-3, 4 = sprites). O `saturn_bios.bin` precisa estar no
diretório atual. O quadro `N` é despejado no VBlank-in, então `--frames N`
escreve até o quadro `N-1`.

## Arquitetura

Camadas, cada uma dependendo só para baixo: `main` → `machine` (raiz de
composição, o único lugar que conhece tipos concretos) → `cpu` / `bus` /
`devices` / `video`.

- **`src/cpu/`** — `state.rs` (`Sh2State`, `#[repr(C)]`), `decode.rs` (`u16` →
  `Insn`, sem executar), `jit/backend/x64.rs` (compilador de bloco, x86-64;
  único backend hoje — `jit/backend/mod.rs` seleciona por `cfg(target_arch)` e
  documenta o que um segundo backend, ARM64 por exemplo, precisaria
  implementar), `jit/mod.rs` (cache de blocos e invalidação, não conhece
  arquitetura nenhuma), `sh2_bus.rs` (trait `Sh2Bus`, `Sh2Runtime` e
  os auxiliares `rt_*`), `address_space.rs` (decodificação A31-A29 do SH-2:
  espelhos cache-through, purge, tag/dados do cache, registradores internos),
  `onchip.rs` (registradores do SH7604; DIVU e FRT reais, o resto são bancos de
  leitura/escrita), `mod.rs` (`Sh2Cpu`: executor de blocos, entrada de exceção e
  interrupção).
- **`src/bus/`** — `MemoryDevice` (por deslocamento, big-endian por padrão) e
  `SystemBus` (registro por páginas de 64 KiB sobre o espaço de 29 bits; também
  marca os trechos com código compilado para invalidação e registra acessos não
  mapeados).
- **`src/devices/`** — reais: `scu.rs` (interrupções IST/IMS, DMA 0-2 direto e
  indireto), `vdp1.rs`, `vdp2.rs`, `ram.rs`, `scsp.rs` (registradores, 32 slots
  de PCM, temporizadores) e `sound_cpu.rs` (o 68000 que roda o driver de som da
  BIOS, do crate `m68k`). Stubs: `smpc.rs`, `cd_block.rs`, `scu_dsp.rs`,
  `stub.rs` (`RegisterStub`, `OpenBus`). O `scsp_dsp.rs` (DSP de efeitos) é
  validado contra a captura real por `src/bin/dsp_check.rs`.
- **`src/video/`** — `egl.rs` (contexto headless, usado pelo gravador de PNG e
  pelo gate), `renderer.rs` (duas passadas GL; `in_current_context()` desenha no
  contexto de quem chama, que é como a janela funciona),
  `dumper.rs` (`FrameSink` → PNG). `timing.rs` gera os eventos de HBlank/VBlank
  (263 linhas × 1820 ciclos).
- **`src/debug/`** — `trace_check.rs`, `break_trace.rs`, `video_state.rs`.
  `src/bin/compare.rs`, `src/bin/gl_probe.rs`, `src/bin/m68k_probe.rs` (roda o
  driver de som da BIOS no núcleo 68000 do crate `m68k`, fora do emulador — ver
  `docs/sound.md`).

### Invariantes do JIT (quebre um e a BIOS erra em silêncio)

- Assinatura do bloco: `extern "C" fn(*mut Sh2State, *mut Sh2Runtime) ->
  next_pc`. Dentro do bloco: `r14` = estado, `r15` = runtime, `rbx` guarda o
  alvo do desvio através do delay slot, `r12`/`r13` guardam valores através das
  chamadas `rt_*`. O prólogo empilha 5 registradores, e é isso que mantém a
  pilha alinhada em 16 bytes para chamadas. **Só x86-64 System V.**
- O layout de `Sh2State` é carga estrutural: ordem dos campos e `repr(C)`; o JIT
  usa constantes de `offset_of!`. O `rte` estaciona o SR restaurado em
  `exit_arg` (memória) durante o delay slot, porque instruções de slot
  (`mac.l`, `tas.b`, `and.b`) usam `r12`/`r13`.
- Um bloco termina depois de desvio + delay slot, `ldc …,SR`, `trapa`, `sleep`,
  opcode ilegal, ou 64 instruções. Interrupções só são aceitas entre blocos.
  Opcode ilegal sai com `EXIT_ILLEGAL` e nunca cai em outra coisa.
- `div1`, `mac.l/.w`, `dmuls/dmulu`, `addc/subc/negc` e as rotações são asm
  inline (a matemática 3D da BIOS depende delas). O `div1` é validado contra a
  divisão nativa em 300 entradas aleatórias.
- Código escrito na RAM invalida blocos pelos trechos sujos do `SystemBus`
  (granularidade de 256 B, endereços canônicos para os espelhos coincidirem).

### Pipeline de vídeo

Passada 1 percorre a lista de comandos do VDP1 (campo JP: próximo, salto,
chamada, retorno; recorte de sistema e de usuário, coordenadas locais; sprites,
sprites distorcidos, polígonos; modos de cor 0-5; gouraud) para um FBO `R16UI`.
Passada 2 é um fragment shader só: NBG0-3 (célula e bitmap), camada de sprites,
cor de fundo, prioridades, cálculo de cor de duas camadas, deslocamento de cor.

Fatos conquistados a duras penas, todos confirmados por quadros com erro zero:
- **As capturas estão guardadas invertidas na vertical** (última linha
  primeiro). O renderizador mapeia a linha 0 do GL para a linha natural 223 e
  escreve a saída do `glReadPixels` sem inverter, então os despejos batem byte a
  byte com as capturas.
- A expansão de cor de 5 para 8 bits é `v << 3`, não `(v<<3)|(v>>2)`.
- No `BGON`, o bit TPON **0 liga** a transparência da cor 0; 1 desliga.
- Os registradores de mapa dos NBG são `MPABN0=0x40, MPCDN0=0x42,
  MPABN1=0x44…` (**passo 4 por NBG**); a unidade de endereço de plano é
  `(64·64·tamanho do PN)`, isto é `0x4000` para PNs de 2 palavras.
- No `CCCTL`, o bit n liga o cálculo de cor do NBG n; a razão tem 5 bits em
  `CCRNA`/`CCRNB`; o resultado é `(topo·(32−r) + segundo·r)/32`.

## Os traces de referência — o que eles são e o que não são

`bios_trace_*.txt` e `branch_trace_*.txt` são **amostra, não espelho**: só a
primeira execução de cada PC, sem o delay slot de desvio tomado, começando no
meio da execução (primeira linha `200003BA`, já na segunda volta do laço; o
reset real é `PC=0x20000200`, `SP=0x06002000`). Os valores de operando são os
registradores **antes** da instrução, na forma `((rN)0xV)`. A chave de dedupe é
`pc & 0x0FFFFFFE`. Por isso o `--trace-check` reporta cobertura, opcode no PC e
desvio mediano de quadro, nunca um diff de linhas.

**"Primeira execução de cada PC" é fato verificado, não suposição:** os dois
traces não têm **nenhum** par (núcleo, PC) repetido, e o quadro máximo é 574 (sem
disco) e 735 (com disco), dentro do limite de 740 do gerador.

Os rótulos de chip na coluna `| Mem:` **estão errados** em parte do mapa nos
traces que temos hoje. Eles não vêm do roteamento do emulador: vêm de uma tabela
de faixas escrita à mão, paralela ao caminho real de memória, e deslocada de uma
região. O patch em `tools/trace-capture/` corrige isso para capturas novas, mas a
regra continua valendo: **o endereço manda**. `0x0580xxxx` é CD Block (rotulado SCSP), `0x05Axxxxx` é RAM do SCSP
(rotulado CD Block), `0x05Bxxxxx` são registradores do SCSP (rotulado SCU/DMA),
`0x05FExxxx` são registradores do SCU (rotulado VDP2), `0x05F0–5F7xxxx` é CRAM
do VDP2. Escritas `mov.w` nunca são anotadas.

## O ciclo de trabalho que funciona

1. Rode e veja onde trava: `--profile` acha o laço de espera.
2. `--break <pc> --break-n <grande>` mostra o que aquele laço consulta.
3. Ache o mesmo PC no trace de referência (`grep "PC: 0601326"`) para ver **que
   valor a máquina real leu ali**.
4. Faça o stub devolver esse valor, e repita. A lista `Report::missing_runs` do
   `--trace-check` aponta a próxima parada.
5. Meça de novo. Se o erro médio não caiu e a porcentagem do trace não subiu,
   não foi a correção.

Cada stub existe para devolver o que a referência leu. Ao criar um, escreva de
onde veio o valor: lido do trace, ou palpite ainda não verificado. Um palpite
anotado é dívida; um palpite disfarçado de fato é armadilha.

## Antes de dar trabalho por pronto

```bash
bash tools/quality_gate.sh
```

São 9 passos determinísticos, sem modelo e sem rede: formatação, catraca de
avisos do clippy, testes, testes que não afirmam nada, regras do projeto,
cobertura das linhas que você mudou, erro médio do vídeo contra as capturas,
porcentagem do trace real reproduzida, e correlação do envelope de áudio contra
uma captura real (`stubs/captures/audio/boot.wav`). Detalhe de cada passo e dos
limiares em `docs/quality-gate.md`.

**Um passo vermelho é um resultado, não um obstáculo.** As únicas correções
honestas: escrever o teste, consertar o código, ou baixar o teto de avisos
quando ele melhorar. Nunca `cargo clippy --fix` (ele já transformou uma guarda
de opcode em no-op num projeto irmão enquanto calava o aviso que apontava o
problema), nunca `--exclude-files`, nunca afrouxar um limiar para ficar verde.
Apertar é livre; afrouxar exige `MIMAS_OVERRIDE_REASON`, e o gate diz no resumo
que aquele verde saiu sob limiar afrouxado.

Ao reportar, diga o que aconteceu: "6 de 8 verdes, cobertura e vídeo vermelhos"
é útil; "o gate passou", quando um limiar foi afrouxado ou um passo pulado, não
é.

## Onde fica o estado do projeto

| o que | onde |
|---|---|
| resultado atual do vídeo e do trace, lacunas conhecidas | `docs/status.md` |
| performance medida, e as desconfianças de otimização e de som | `docs/current_status.md` |
| o que cada stub modela e de onde veio cada valor | `docs/stubs.md` |
| o briefing original do projeto | `docs/mission-brief.md` |
| como capturar trace novo a partir de uma BIOS sua | `tools/trace-capture/README.md` |
| som: o que já foi medido, o desenho da thread e a ordem de trabalho | `docs/sound.md` |
| passos e limiares do gate | `docs/quality-gate.md` |

## Arquivos na raiz que não fazem parte do build

`legacy_src/` (interpretador e código glium antigos), `dump_loop.rs`,
`dump_data.rs`, `test_offset.rs`, `test_dynasm.rs`, `decode.s`, `gen_sh2.py`,
`generate_sh2.py`, `scratch.py`, `instruction_sh2.md` (sobras do fluxo de
geração do interpretador; o `generate_sh2.py` sobrescreveria um `sh2.rs`
obsoleto), `run_log.txt`. Todos estão no `.gitignore`.

`saturn_bios.bin` e os quatro traces estão ignorados de propósito: o
repositório é público, e eles **são** o programa da BIOS — o trace traz opcode e
desmontagem de cada instrução executada. Precisam existir em disco para o gate
medir; nunca force a entrada deles no git.

`stubs/captures/` (68 quadros da BIOS, 1,2 MB) está **versionado**: é o oráculo
do projeto, são pixels de saída e não contêm código. Sem eles o passo 7 do gate
não roda. Já `stubs/captures-game/` são 116 quadros de gameplay de um jogo
comercial, fora do escopo e ignorados.

`stubs/captures/audio/boot.wav` também está **versionado**: 12 s de PCM,
gravados por loopback da saída de áudio do YabaSanshiro instrumentado, rodando
a BIOS real dentro do RetroArch (a exceção da regra 4: saída de um emulador
instrumentado, não o código dele — não é hardware real, é a mesma referência
usada para o trace e para o DSP). Sem ele o passo 9 do gate não roda. Ver a
proveniência completa e as lacunas conhecidas em `docs/sound.md`.
