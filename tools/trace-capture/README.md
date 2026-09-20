# Como capturar os traces de referência

O repositório não traz a BIOS nem os traces: os dois **são** o programa da BIOS.
Esta pasta traz o necessário para você produzir os seus, a partir de uma BIOS
que você já tenha.

O resultado são dois arquivos por execução:

- `bios_trace.txt` — a **primeira execução de cada PC**, em ordem, com quadro,
  núcleo (M ou S), opcode, desmontagem e o acesso à memória daquela instrução;
- `branch_trace.txt` — a primeira ocorrência de cada aresta de desvio
  (origem → destino).

São eles que o `mimasv2 --trace-check` consome e que sustentam o passo 8 do
`tools/quality_gate.sh`.

## Aviso sobre a diretiva 4

A regra do projeto é **não consultar emuladores vizinhos**. Capturar trace é a
exceção explícita e delimitada: você aplica um patch de instrumentação, roda, e
usa a **saída**. O código do emulador não entra aqui, e nada em `src/` pode
derivar dele. `tools/project_rules.py` reprova qualquer vocabulário de
implementação do Yabause dentro de `src/`.

## Passo a passo

O patch é contra o [YabaSanshiro](https://github.com/devmiyax/yabause) e toca
três arquivos: `yabause/src/sh2int.c` (o interpretador de SH-2),
`yabause/src/memory.c` (os acessos à memória) e
`yabause/src/libretro/libretro.c` (uma linha, para permitir bandeja vazia).

```bash
git clone https://github.com/devmiyax/yabause
cd yabause
patch -p1 < <caminho>/tools/trace-capture/yabasanshiro-trace.patch
```

Compile o núcleo libretro (ou qualquer porta que use o **interpretador**, não o
dynarec — a instrumentação vive no interpretador):

```bash
cd yabause/src/libretro && make -j$(nproc)
```

Rode com o **interpretador** selecionado. Os arquivos saem no diretório de
trabalho do processo, com o prefixo que você escolher.

### Capturando o boot da BIOS

O padrão já é esse: quadros 0 a 740, prefixo `bios`.

```bash
MIMAS_TRACE_NO_DISC=1 ./emulador jogo.chd      # bandeja vazia → CD Player
mv bios_trace.txt bios_trace_no_game.txt
mv bios_branch_trace.txt branch_trace_no_game.txt

./emulador jogo.chd                            # com disco
mv bios_trace.txt bios_trace_with_game.txt
mv bios_branch_trace.txt branch_trace_with_game.txt
```

**Sobre o cenário sem disco:** o núcleo libretro recusa carregar sem conteúdo
(`retro_load_game` devolve `false` quando `info` é nulo), então continue
passando um arquivo. Com `MIMAS_TRACE_NO_DISC=1` o patch troca o núcleo de CD
pelo `CDCORE_DUMMY`, que responde "CD not present" (`cdbase.c`, `dmy_status =
2`). É a bandeja vazia de verdade, e é o caminho que leva a BIOS ao **CD
Player** — menu completo, marquee e o gestor de dados internos, que é bastante
hardware exercitado de graça.

Ponha os quatro na raiz deste repositório. O `.gitignore` já os mantém fora do
git.

### Capturando um jogo

Pule o boot e não ponha teto: o que interessa começa depois da BIOS.

```bash
MIMAS_TRACE_PREFIX=mkr \
MIMAS_TRACE_MIN_FRAME=740 \
MIMAS_TRACE_MAX_FRAME=4294967295 \
  ./emulador --bios saturn_bios.bin --disc jogo.chd
# → mkr_trace.txt e mkr_branch_trace.txt
```

Jogue os trechos que você quer cobrir: o trace só registra a **primeira**
execução de cada PC, então o arquivo cresce quando código novo roda e fica
parado quando o jogo repete o que já fez. Menu, primeira fase e uma luta dão
traces bem diferentes.

| variável | padrão | para que serve |
|---|---|---|
| `MIMAS_TRACE_PREFIX` | `bios` | nome dos arquivos: `<prefixo>_trace.txt` e `<prefixo>_branch_trace.txt` |
| `MIMAS_TRACE_MIN_FRAME` | `0` | primeiro quadro capturado; use para pular o boot |
| `MIMAS_TRACE_MAX_FRAME` | `740` | último quadro; suba para capturar jogo |
| `MIMAS_TRACE_NO_DISC` | desligado | `1` força a bandeja vazia (o caminho do CD Player) |
| `MIMAS_DSP_CAPTURE` | desligado | `1` liga a captura do DSP de efeitos (abaixo) |
| `MIMAS_DSP_ROWS` | `60000` | quantas amostras de entrada/saída do DSP gravar |

### O DSP de efeitos

O som de boot da BIOS sai **só** pelo DSP de efeitos: o slot chega com o envio
seco em zero, então o que se ouve é o retorno do DSP e nada mais. Com
`MIMAS_DSP_CAPTURE=1` o patch grava cinco arquivos, **todos do mesmo instante**:

```bash
MIMAS_DSP_CAPTURE=1 MIMAS_TRACE_PREFIX=mk MIMAS_DSP_ROWS=20000 \
  retroarch -L .../yabasanshiro_libretro.so jogo.chd
```

| arquivo | o que traz |
|---|---|
| `<prefixo>_dsp_program.txt` | `rbp`/`rbl`, os 64 coeficientes, os 32 atrasos e os 128 passos de microcódigo |
| `<prefixo>_dsp_state.txt` | `mdec_ct`, `shift_reg`, os 128 TEMP, os 32 MEMS e os registradores de acesso |
| `<prefixo>_dsp_ram.bin` | 512 KiB de RAM de som — é onde mora o buffer circular do reverb |
| `<prefixo>_dsp_io.txt` | uma linha por amostra: índice, 16 entradas do mixer, 16 saídas de efeito |
| `<prefixo>_dsp_steps.txt` | estado passo a passo (`shift_reg`, `io_addr`, `read_value`, `inputs`) das 4 primeiras amostras |

**Por que os cinco juntos, e por que isso importa:** o driver de som recarrega o
programa do DSP durante o boot. Um dump de programa tirado num momento e um de
estado tirado em outro não se comparam — foi exatamente assim que uma caçada a
um bug de matemática se perdeu por horas, quando o programa capturado tinha
`rbp 11` e o que rodava tinha `rbp 24`.

O estado inicial resolve o outro problema: o primeiro atraso deste reverb é de
11.263 amostras (255 ms). Começando com o anel vazio, a saída fica em zero por
um quarto de segundo, e a comparação nessa janela só mede o silêncio. Carregado
o estado da referência, dá para comparar desde a amostra 0.

Do lado do mimasv2, quem consome isso é `src/bin/dsp_check.rs`:

```bash
DSP_STATE=mk_dsp_state.txt DSP_RAM=mk_dsp_ram.bin DSP_STEPS=mk_dsp_steps.txt \
  ./target/release/dsp_check mk_dsp_program.txt mk_dsp_io.txt
```

Ele carrega o estado, roda o nosso DSP com as entradas da referência e reporta o
primeiro passo em que os dois discordam. `0 divergentes` por amostra quer dizer
que o núcleo bate instrução a instrução; o que sobra depois disso é rota de
sinal, não matemática.

### Quando a captura morre no meio

Os arquivos são descarregados a cada mil linhas, e o patch instala ganchos de
saída e de sinal (`SIGSEGV`, `SIGBUS`, `SIGILL`, `SIGFPE`, `SIGABRT`, `SIGINT`,
`SIGTERM`). Se o emulador cair ou for morto, o trace termina com uma linha que
diz o que houve:

```
# fim: sinal 11 | ultimo Frame: 574 | Core: M | PC: 00003958
```

O gancho re-lança o sinal com o tratador padrão depois de descarregar, então o
crash continua aparecendo como sempre apareceu. O `--trace-check` ignora linhas
que começam com `#`.

**Isto existe por um motivo concreto.** O `bios_trace_no_game.txt` que temos
termina **no meio de uma linha**, sem quebra de linha final, no quadro 574 — e o
`branch_trace_no_game.txt` também. Os dois arquivos com disco terminam limpos.
Ou seja: aquele run morreu durante a escrita, e não se sabe por quê, porque a
versão anterior da instrumentação não registrava nada ao morrer e ainda perdia
o buffer. Com este patch, o próximo run sem disco diz se foi segfault, aborto ou
encerramento pelo usuário, e em que PC.

Passo opcional, para leitura humana:

```bash
python3 tools/trace-capture/decode_trace.py bios_trace_no_game.txt > decoded.txt
```

## O que o patch faz

- **`sh2int.c`**: antes de executar cada instrução, marca o PC num bitmap por
  núcleo (`mimas_visited_pcs`, 128 MB de `calloc`, alocado só quando a captura
  começa); na primeira vez que aquele PC roda naquele núcleo, desmonta e escreve
  a linha. Depois de executar, compara o PC novo com `pc + 2` para detectar
  desvio e registra a aresta, deduplicada por uma tabela de hash aberta de 128 K
  entradas.
- **`memory.c`**: `LogMemAccess` guarda o último acesso mapeado num buffer
  global que a linha do trace consome. As seis acessoras mapeadas (byte, word e
  long, leitura e escrita) são instrumentadas. `MimasTraceWindow()` é a janela
  de quadros, lida do ambiente uma vez.
- **`libretro/libretro.c`**: uma escolha, na inicialização, entre o núcleo de CD
  normal e o `CDCORE_DUMMY`, conforme `MIMAS_TRACE_NO_DISC`. Nenhuma outra
  mudança de comportamento.

Ambos os núcleos são capturados, e a coluna `Core:` diz qual (`M` ou `S`). Isso
importa para jogo: a BIOS quase não usa o Slave, e os jogos usam.

## Cinco correções em relação à versão que gerou os traces em circulação

Os traces que existem hoje foram feitos com uma versão anterior desta
instrumentação. Se você regenerar, a sua saída será **melhor**, e diferente
nestes pontos:

1. **A tabela de desvios não trava mais o emulador.** Na versão anterior, quando
   as 128 K entradas enchiam, a busca por espaço livre (`while (branch_from[hash]
   != 0)`) girava para sempre, travando a emulação. O boot da BIOS não chega
   perto do limite; **um jogo chega**. Agora a busca é limitada, e ao encher a
   tabela o registro de desvios para com um aviso, sem afetar o resto.
2. **Rótulos de chip corrigidos.** A tabela antiga estava deslocada de uma
   região: chamava o CD Block de "SCSP/Audio RAM", a RAM de som de "CD Block",
   os registradores do SCSP de "SCU/DMA" e os do SCU de "VDP2 Regs". Ela é
   escrita à mão e **paralela ao roteamento real do emulador**, então nunca foi
   o emulador dizendo para onde o endereço ia. Duas derivações independentes
   chegaram à mesma correção: o mapa físico do Saturn, e o próprio código do
   emulador (o `scu.c` implementa o `RSEL` em `0xC4`, provando que `0x05FExxxx`
   é SCU e não VDP2). Também ganhou as faixas que só jogo usa: cartucho
   (A-Bus CS0/CS1), backup RAM, MINIT/SINIT e o framebuffer do VDP1 separado da
   VRAM. Faixa não listada continua sem rótulo, em vez de receber um errado.
3. **Escritas de palavra passam a ser anotadas.** `MappedMemoryWriteWord` era a
   única acessora sem chamada, e é a origem da esquisitice documentada de que
   "`mov.w` nunca aparece anotada".
4. **Chamada duplicada removida, e o arquivo é descarregado periodicamente.**
   Cada acessora chamava `LogMemAccess` duas vezes seguidas; e nada era
   descarregado antes do fim do processo.
5. **A captura sobrevive ao crash e diz onde parou**, e existe um modo de
   bandeja vazia de verdade (`MIMAS_TRACE_NO_DISC=1`) em vez de depender de
   como o frontend foi configurado. As duas coisas atacam o mesmo problema: o
   trace sem disco atual acaba no meio de uma linha, sem explicação.

Nada disso muda as colunas de quadro, núcleo, PC e opcode, que são as que o
`--trace-check` lê. Um trace regenerado continua comparável com os pisos do
gate.

## O que o trace é, e o que ele não é

**Amostra, não espelho.** Só a primeira execução de cada PC: laços aparecem uma
vez, e um bloco de registradores escrito por um laço aparece só no primeiro
endereço. O delay slot de um desvio tomado costuma faltar. A captura começa no
meio da execução (primeira linha `200003BA`, já na segunda volta do laço),
enquanto o reset real é `PC=0x20000200`, `SP=0x06002000`.

Por isso o `--trace-check` reporta cobertura, opcode no PC e desvio mediano de
quadro — nunca um diff linha a linha.

## Verificação feita aqui

O patch **aplica limpo** sobre um checkout intocado do YabaSanshiro
(`patch -p1 --dry-run`), e os dois arquivos passam em `gcc -fsyntax-only -Wall`
sem nenhum erro e sem aviso vindo do código inserido.

O `libretro.c` não foi verificado isoladamente (ele depende do resto da árvore
para compilar); a mudança ali é uma escolha entre dois valores já usados pelo
próprio arquivo.

**O que ainda não foi feito:** compilar o emulador inteiro e capturar de ponta a
ponta com esta versão. Três coisas só se provam na primeira captura:

1. se a tabela de desvios encher num jogo, deve sair aviso no stderr em vez de a
   emulação travar;
2. o run sem disco deve chegar ao CD Player, e se morrer de novo no quadro 574,
   a linha `# fim:` dirá o motivo;
3. os rótulos novos (cartucho, backup RAM, framebuffer do VDP1) só aparecem em
   trace de jogo.
