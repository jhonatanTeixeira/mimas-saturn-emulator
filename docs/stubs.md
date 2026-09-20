# Os stubs e o que cada um modela

Só vídeo, e o que o vídeo depende, é real. Todo o resto devolve o mínimo para a
sequência da BIOS seguir. Cada valor aqui tem procedência declarada: **lido do
trace real** ou **palpite ainda não verificado**. Um palpite anotado é dívida;
um palpite disfarçado de fato é armadilha.

## CD block (`0x05800000`)

- Relatório de status no reset: `CR1=0x2100 CR2=0x4101` — PAUSE com o bit de
  periódico, faixa de dados 1. **Lidos do trace de referência.**
- `CR3=0x0100 CR4=0x0096` — índice 1, FAD 150. **Palpite plausível, não
  verificado.**
- Qualquer comando ecoa o relatório com `HIRQ.CMOK`.
- `HIRQ.SCDQ (0x400)` pulsa a cada 1/75 s: o laço de retentativa da BIOS em
  `0x25F4` precisa de 3 deles.

## SMPC

- Comandos completam instantaneamente.
- `INTBACK` devolve RTC fixo, código de área `0x04` (América do Norte,
  **inferido** do hash e das strings da BIOS e do jogo americano nas capturas),
  nenhum controle conectado, e então levanta a interrupção de SMPC no SCU.

## RAM do SCSP (`SoundRam`)

A BIOS conversa com o driver de som do 68000 por uma caixa de correio: o byte de
comando no deslocamento `0x700` precisa ler de volta `0`. Qualquer escrita (byte,
palavra ou long) que cubra `0x700` limpa o byte — ou seja, o driver consome o
comando na hora. Registradores do SCSP e RAM de backup são `RegisterStub`
comuns.

## DSP do SCU (`scu_dsp.rs`)

As portas PPAF/PPD/PDA/PDD são reais. A execução é por **reconhecimento do
único programa de 32 palavras que a BIOS carrega**: uma cópia em blocos de
`data[2]` palavras, de `data[0]<<2` até `data[1]<<2`, através de um banco de 64
palavras. Foi decodificado à mão — ele copia VRAM do VDP1 para a RAM do SCSP.

Um programa desconhecido é registrado em `dsp.unknown` e logado alto, em vez de
ser aceito em silêncio. **Se um programa novo aparecer, é sinal de que um
interpretador de DSP de verdade passou a ser necessário.**

## A-Bus, MINIT e SINIT

`OpenBus(0xFF)`. O MINIT e o SINIT são como um SH-2 interrompe o outro; enquanto
não houver Slave, não há o que fazer com eles.
