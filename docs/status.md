# Estado atual

Este arquivo muda a cada sessão. Os arquivos de agente (`CLAUDE.md`,
`GEMINI.md`) não repetem nada daqui, de propósito: número em arquivo de regra
envelhece e vira ruído.

**Medido em 2026-09-20**, `cargo build --release`, nesta máquina.

## Vídeo contra as capturas reais

```
./target/release/mimasv2 --frames 620 --dump out
./target/release/compare stubs/captures out --max-frame 728
```

**Erro médio geral: 1,66/255** (620 quadros gerados, 68 capturas até 728).
Execução: 5,4 s, 50,8 milhões de blocos, 15.580 compilados, 54,5 Mciclos/s.

Por faixa, erro máximo:

| faixa | quadros | erro | o que é |
|---|---|---|---|
| 0–257 | 31 | **0,00** | tela preta |
| 264–372 | | 1,3 | primeiros estilhaços |
| 478–497 | | 6,3 | montagem (o quadro 497 sai exato) |
| 515–516 | | 4,9 | flash |
| 555–707 | 14 | **0,00** | logo da SEGA, bit a bit |
| 708–728 | | 13,9 | fade-out e licença, não reproduzidos |

Os nossos números de quadro correm adiantados em relação às capturas (Δ mediano
−191 nesta medição), então o `compare` procura em todos os despejos em vez de
casar por número.

## Execução contra o trace real

```
./target/release/mimasv2 --frames 700 --trace-check bios_trace_no_game.txt
```

**6685 de 7256 PCs da referência visitados (92,1%), 0 opcodes divergentes**,
5043 PCs nossos fora da referência, desvio mediano de quadro −156.

Primeiro PC da referência que não visitamos: **`0x000033E4`, quadro 181 de
referência** (o anterior na referência é `0x000040D8`). São 24 faixas ausentes
no total.

## Som

Medido em 2026-09-19, detalhe e método em `docs/sound.md`:

- o driver de som real da BIOS **roda** no núcleo 68000 do crate `m68k` 0.14.0:
  222.897 instruções, nenhuma falha, **zero acessos fora do mapa**;
- ele inicializa os 32 slots do SCSP (1.162 registradores, 1.863 escritas),
  consome a caixa de correio e para no laço de espera em `0x11FC`;
- velocidade: **193 Mciclos/s** com o interpretador, sem o JIT do crate — 17
  vezes o tempo real do 68000 do Saturn, uns 6% de um núcleo desta máquina;
- nada disso ainda toca: faltam os temporizadores do SCSP, os comandos da BIOS
  e a mixagem. E a exatidão do crate ainda não foi conferida contra um trace
  real do 68000.

### Som dentro do emulador (2026-09-19, noite)

A BIOS boota com som, em tempo real, na janela `live`. O driver real roda no
68000 e programa o SCSP; a nota toca de 0,634 s a 8,779 s do boot. Vídeo e trace
não regrediram (1,66 e 92,1%).

Falta, com causa identificada: a reverberação (DSP de efeitos, não implementado
— o slot está roteado para ele) e o segundo som do boot (só uma chave de slot é
ligada no boot inteiro). Detalhe em `docs/sound.md`.

**Cobertura daquela mudança: 13%.** O gate mede e reprova, como deve. A maior
parte do que falta é fiação (`machine.rs`), interface de linha de comando
(`main.rs`) e o caminho GL (`renderer.rs`), que não têm teste de unidade. O SCSP
e o 68000 têm: 12 testes com valores derivados à mão.

**Correção no próprio harness:** o passo de cobertura rodava o tarpaulin com o
perfil de debug do projeto, que compila com `opt-level = 1`. Com otimização ele
perde a atribuição de linha e media 13 linhas de uma mudança de 113 — dando 85%
onde a verdade era 13%. Agora roda com `CARGO_PROFILE_DEV_OPT_LEVEL=0`.

### DSP de efeitos e mixagem (2026-09-20)

O DSP de efeitos **bate com a referência bit a bit**: 99,7% de amostras idênticas
em 60.000, pico 4004 igual, correlação 1,000000, e **0 passos divergentes em
108** no trace passo a passo das quatro primeiras amostras. Medido por
os testes de `src/devices/scsp_dsp.rs` e, de fora, o envelope de áudio
contra `stubs/captures/audio/boot.wav`. O comparador passo a passo contra a
captura de um emulador foi retirado: era estado interno de chip.

Quatro defeitos reais caíram no caminho:

| defeito | efeito |
|---|---|
| `last_step` do DSP (rodávamos 128 passos; o hardware para no último não-zero) | `shift_reg` errado no fim de cada amostra |
| `io_addr` só recalculado em passos com `mrd`/`mwt` | trace por passo não fechava |
| nível de interrupção fixo em 4, ignorando `SCILV0/1/2` | 68000 entrava pelo autovetor errado; driver ia a 2,59 M em vez de 11,08 M instruções e nunca subia o programa do DSP |
| `SoundRam` zerava o byte de comando da caixa de correio | BIOS lia "consumido" para comandos que o driver nunca viu |

A cadeia de mixagem passou a ser a do hardware, em inteiros: seco por DISDL, envio
ao DSP por **IMXL/ISEL** (era EFSDL, registrador errado), retorno por EFSDL/EFPAN
do slot, mestre por MVOL. Pico da saída caiu de **32767 (saturando) para 7528**.

**2026-09-20: envelope por tempo.** O toque não se arrasta mais — um slot segura
o volume por 50 ms e decai numa taxa fixa (medido com `--dump-audio`: pico
2214 em 0,8 s, 36 em 2,0 s, piso quase inaudível por volta de 2,6 s). Não é o
envelope do hardware (não lê AR/D1R/D2R/RR/KRS): é um relógio fixo, não um
registrador. Ver `docs/sound.md`.

**Gate em 2026-09-20 (depois do envelope por tempo): 8 verdes, 0 vermelhos.**
76 testes, 28 avisos de clippy (teto), cobertura do diff 100% (7/7 linhas novas
em `scsp.rs`, cobertas pelo teste do envelope). Vídeo (1,66/255) e trace
(92,1%) não regrediram.

## Lacunas conhecidas

- **Fade-out (708–709), preto (710–713) e tela de licença (720+)** precisam de
  um disco detectado; não reproduzidos. Depois do logo, a nossa BIOS fica
  parada nele.
- **Numeração de quadro adiantada.** A BIOS real queima tempo em timeouts (poll
  do CD, esperas do driver de som) que nós encurtamos ou cobramos barato demais.
  Não há estados de espera de barramento modelados: a referência gasta ~330
  ciclos por volta do poll de CD, nós ~227.
- **Quadros de detritos 478–486 (erro ≈ 6) e flash 515 (4,9):** faltam recursos
  do VDP1 (meia luminância, sombra, meia transparência, malha, redução em alta
  velocidade), cálculo de cor da camada de sprites do VDP2, sombra, janelas,
  scroll por linha, zoom e RBG0. O desenho do VDP1 é instantâneo no VBlank-in (o
  hardware real tem um quadro de latência), e quadriláteros usam dois
  triângulos, exato para paralelogramos e aproximado para distorcidos em geral.
- **Modos 1 e 2 de RAM de cor do VDP2 e nomes de padrão de 1 palavra** estão
  escritos, mas não testados.
- **Gerador de envelope do SCSP não é o do hardware.** O toque decai (ver
  acima), mas por um relógio fixo, não pelas taxas de AR/D1R/D2R/RR/KRS. Falta
  um oráculo — captura de uma curva de envelope real — antes de decodificar
  esses registradores; sem ele seria palpite travestido de fato.
- **28 avisos do clippy** (código morto de andaime). É o teto atual da catraca
  do gate; quando cair, baixe `MIMAS_MAX_WARNINGS`.
- **Cobertura da árvore inteira em 2,4%** (385 de 16.207 linhas contadas pelo
  tarpaulin; a expansão das macros do dynasm infla o denominador). O gate mede
  cobertura só das linhas que a mudança toca; a cobertura da árvore é dívida
  registrada, não medida a cada rodada.
