# Som: o que já foi medido e por onde seguir

## O desenho do hardware, que decide o nosso

A BIOS toca som durante a animação de polígonos, mas **ela quase não fala com o
SCSP**. No trace real, a SH-2 escreve num único registrador do chip de som:
`0x25B00400`, no quadro 188 — o controle comum. Todo o resto é feito por um
**driver de 68000** que ela carrega na RAM de som e acorda com o `SNDON` do
SMPC. Os dois conversam por uma caixa de correio em `0x700` da RAM de som.

Consequência direta: **sem 68000 não há som**, e o oráculo do som não é a onda,
é o que o driver escreve nos registradores do SCSP.

## Experimento de 2026-09-19: o driver real roda no crate `m68k`

`mimasv2 --dump-sound-ram <arquivo>` despeja os 512 KB da RAM de som ao fim da
execução. Depois de 300 quadros, ela contém 108.716 bytes não-zero e os vetores
de reset `SP=0x0000A000 PC=0x00001000` — o driver da BIOS, carregado.

`cargo run --release --bin m68k_probe -- <arquivo> [ciclos]` roda esse driver no
núcleo 68000 do crate [`m68k`](https://crates.io/crates/m68k) 0.14.0 (MIT),
com a RAM de som em `0x000000` e os registradores do SCSP em `0x100000`.

Resultado:

| medida | valor |
|---|---|
| instruções executadas | 222.897 em 2 milhões de ciclos |
| falhas, opcodes inválidos | nenhuma |
| acessos fora do mapa | **0** — o mapa que supusemos é o que o driver usa |
| registradores do SCSP tocados | 1.162 distintos, 1.863 escritas |
| caixa de correio depois | `0x700..0x710` zerada: o driver consumiu o comando |
| PC final | `0x11FC`, em laço de espera |
| velocidade | 113 Mciclos em 0,585 s = **193 Mciclos/s** |

O 68000 do Saturn roda a ~11,3 MHz, então isso é **17 vezes o tempo real, com o
interpretador, sem o JIT do crate ligado** — algo como 6% de um núcleo desta
máquina. Fica medido, e não suposto, que **a CPU de som não é o custo do
áudio**; o custo é a mixagem do SCSP, 32 slots a 44,1 kHz.

### O que esse experimento **não** prova

- O driver só fez **inicialização**: zerou os 32 slots e parou no laço. Para
  tocar, ele precisa das interrupções de temporizador do SCSP e dos comandos que
  a BIOS manda pela caixa de correio.
- A exatidão do crate ainda não foi conferida contra nada. A verificação é a
  mesma da SH-2: um trace de PCs do 68000 tirado do emulador instrumentado,
  comparado instrução a instrução.

## 2026-09-19, mais tarde: o som saindo de dentro do emulador

O 68000 e o SCSP entraram na máquina, e a BIOS agora boota com som numa janela
em tempo real (`cargo run --release --bin live`).

**O que foi construído:**

- `src/devices/scsp.rs` — banco de registradores, 32 slots de PCM com tom,
  volume e pan lidos a cada amostra, três temporizadores e as interrupções que
  eles levantam. Mixagem a 44,1 kHz.
- `src/devices/sound_cpu.rs` — o 68000 do crate `m68k`, com o espaço de
  endereços do Saturn e ritmo derivado dos ciclos da SH-2.
- `SNDON`/`SNDOFF` do SMPC ligam e desligam o 68000; com ele rodando, o stub da
  campainha sai de cena e quem responde é o driver real.
- `src/bin/live.rs` — janela SDL com o contexto OpenGL próprio; o renderizador
  desenha nele e o quadro vai do FBO para a tela por `glBlitFramebuffer`, sem
  passar pela CPU. **O caminho headless EGL continua intacto** e é o que o
  gravador de PNG e o gate usam.

**O que se mediu:**

| medida | valor |
|---|---|
| conversa da caixa de correio | a BIOS manda `0x80`, `0x82`, `0x83`, `0x87`, os parâmetros `50 80 79 07`, depois `0x85`, `0x86` |
| o driver obedece | liga o slot 0 com SA=`0x50000`, tom `0x7942`, nível 7 — exatamente os parâmetros recebidos |
| nota tocada | de 0,634 s a 8,779 s do boot, com laço ligado (`LPCTL=1`) |
| chaves ligadas no boot inteiro | **1** |
| vídeo e trace | inalterados: erro médio 1,66 e 92,1% |

**Dois bugs corrigidos no caminho**, ambos meus:

1. O endereço da amostra de 16 bits era `(SA + índice) × 2`, o que dobra também
   o endereço inicial: o som saía como ruído. O certo é `SA + índice × 2`.
2. O 68000 acelerava sem limite porque o orçamento de ciclos era calculado por
   bloco da SH-2 (1 ou 2 ciclos) e uma instrução de 68000 custa no mínimo 4: o
   estouro nunca era cobrado. Agora há orçamento acumulado com dívida e lote
   mínimo de 64 ciclos.

**O que falta, segundo o ouvido do usuário comparando com o YabaSanshiro:**

O boot real é "prilulilulin **com cauda**, depois shuuuuaan". Aqui sai o
prilulilulin sem cauda, e o shuuuuaan não existe.

- **A cauda é o DSP de efeitos.** O slot que tocamos vem com `DISDL = 0` e
  `IMXL = 7`: ele está roteado para o DSP, não para a saída direta. Nós o
  mandamos seco para não sumir com o som — a reverberação é justamente a parte
  que falta.
- **O segundo som não aparece** porque o driver liga um slot só no boot inteiro.
  A conversa da caixa de correio acontece, então a investigação é: o driver está
  esperando algo que não chega (ritmo do temporizador? outra interrupção?), ou
  ele depende do DSP para o segundo som.

### O que a investigação da noite estabeleceu

Tudo abaixo é medido no nosso emulador rodando o driver real.

- **O driver obedece à BIOS.** A nota que toca usa exatamente os parâmetros que
  a BIOS passou na caixa de correio (`50 80 79 07` → SA `0x50000`, tom `0x7942`,
  nível 7).
- **Ele não mexe na nota depois de ligá-la.** Entre 0,634 s e 8,779 s não há uma
  única escrita nos registradores do slot: nem tom, nem volume, nem pontos de
  laço. A repetição que se ouve é exatamente o que ele programou.
- **O DSP de efeitos não é a fonte da cauda.** O driver escreve os 512 offsets
  da área de microcódigo (`0x800+`) e escreve **zero** em todos. Ele configura o
  buffer circular (`0x402 = 0x0118`) e não carrega programa nenhum. Então a
  cauda que se ouve no console não vem de um programa de DSP carregado nesta
  fase do boot.
- **O bit de interrupção do temporizador estava errado.** O driver habilita
  `SCIEB = 0x0080` e confirma com `SCIRE = 0x0040`. Habilitar um bit e limpar
  outro só faz sentido se o temporizador A morar no bit 7 do registrador de
  habilitação, e não no 6. Depois da correção o driver passa a receber o tique
  que pediu (mais 35 mil instruções executadas) e termina no laço ocioso em
  `0x11F8`, em vez de parado no meio do código.
- **Ainda assim, uma nota só.** A BIOS manda mais comandos depois (`0x85`,
  `0x86`, `0x08`, `0x82`, `0x80`, `0x81`, `0x0A`, `0x05`) e o driver responde a
  eles — ele desliga a nota no comando certo — mas não liga nenhuma outra.

**A captura de referência foi tentada e falhou**, por dois motivos que ficam
registrados para a próxima: o monitor gravado não era o destino que o RetroArch
usava (saiu quase mudo), e o núcleo compilado é o instrumentado, que grava trace
em disco justamente durante o boot e deixa a emulação com lag. Para a próxima:
compilar um núcleo limpo e gravar o monitor do destino correto, confirmado com
`pactl list sink-inputs` enquanto ele toca.

### Conhecimento conferido na fonte de referência (permissão pontual do usuário)

Olhar, não copiar: nada de código veio de lá, e estes são fatos de hardware que
agora estão certos em vez de deduzidos.

- **Campos do registrador de controle do slot:** confirmados exatamente como
  estavam deduzidos aqui — `KYONEX` no bit 12, `KYONB` no 11, `SBCTL` em 10-9,
  `SSCTL` em 8-7, `LPCTL` em 6-5, `PCM8B` no 4 e os quatro bits altos do
  endereço inicial em 3-0.
- **Bits de interrupção:** temporizador A é o bit 6, B é o 7, C é o 8, e o bit
  10 dispara **uma vez por amostra**. A minha "correção" anterior, que movia o A
  para o bit 7, estava errada e foi desfeita; o bit por amostra, que eu tinha
  removido, voltou.
- **Contagem dos temporizadores:** é ponto fixo 8.8. Cada amostra soma
  `256 >> prescaler` e a interrupção sai quando o contador cruza `0xFF00`, então
  o período é `(255 − valor escrito) << prescaler` amostras. O valor escrito é o
  ponto de partida, não um limite.

### A referência de áudio, enfim gravada — e um erro de instrumento no caminho

As duas primeiras gravações pegaram **o microfone**, não a saída: o `pw-record`
ignorou o alvo e usou a entrada padrão. Tudo o que foi concluído delas foi
descartado. O que funciona é `parec -d <destino>.monitor`, e ele foi **validado
antes de ser usado**: tocando um arquivo de pico 6752, a gravação registrou
6721.

Com a referência boa, comparando envoltórias:

| | referência | nosso |
|---|---|---|
| duração do som | ~5 s | 8,1 s |
| forma | ataque, meio baixo, **crescendo no fim**, silêncio | motivo repetindo a cada 1,2 s |
| tom dominante | 817 Hz | 882 Hz |
| energia de agudo (cruzamentos/s) | 2296 | 784 |

O tom está certo — a diferença não é velocidade de reprodução. O que falta é
**conteúdo**: a referência tem três vezes mais agudo e uma forma que só se
explica com mais de uma voz tocando. Aqui o driver liga um slot no boot inteiro.

### O DSP de efeitos entrou, e com ele quatro erros meus vieram à tona

Com a referência gravada e o diagnóstico dela (`avg_active_slots=1.0/32`,
`avg_dsp_steps=82.0`), ficou claro que **o console também toca um slot só** — o
que nos separava era o DSP. Implementado em `src/devices/scsp_dsp.rs`: 128
passos de microcódigo por amostra, buffer circular na RAM de som, aritmética de
24 bits.

Os quatro erros que a implementação revelou:

1. **Seco e efeito estavam trocados.** No mapa de bytes do slot, `0x16` é
   `DISDL/DIPAN` e `0x17` é `EFSDL/EFPAN`. O driver escreve `0x00E0`: **seco
   mudo, efeito no máximo**. Eu lia a palavra de 16 bits e tomava o byte baixo
   como seco — ou seja, tocávamos exatamente o sinal que o hardware cala. Há
   agora um teste que fixa essa distinção.
2. **As faixas de registrador do DSP estavam erradas:** o certo é `COEF` em
   `0x700`, `MADRS` em `0x780` e o **microcódigo em `0x800`**. Por causa disso,
   os coeficientes que o driver escrevia pareciam ser programa.
3. **O envio ao efeito estava 256× baixo**: o barramento de mixagem trabalha em
   20 bits e eu escalava a amostra para baixo.
4. **Os temporizadores** passaram a contar em ponto fixo 8.8, como o chip.

**Onde parou:** o DSP roda 100 passos por amostra (referência: 82) e escreve em
cinco canais de saída, com picos de 4485 a 32760. Mas o que sai ainda não é
reverberação: os canais 0 e 1, que eu mixava, ficam em **corrente contínua**
(zero cruzamentos por segundo), e somar todos os canais dá ruído agudo. O slot
manda seu envio para o **canal 7** da entrada.

Ou seja: o caminho está certo e a matemática ainda não. Os próximos suspeitos,
em ordem: a seleção do par de canais que vai à saída, o acumulador do
deslocador (um sinal preso explica contínua), e o envelope, que segue
simplificado.

## 2026-09-20: o DSP fecha com a referência, e o caminho até ele

Esta sessão levou o DSP de efeitos de "não bate" para **bit a bit igual à
referência**, e no caminho derrubou três defeitos reais fora dele. A ordem
importa, porque dois dos achados foram erros de instrumento meus, não do código.

### O que estava medido errado

A comparação anterior (`dsp_check`) dizia 1,2% de amostras iguais e saída zero.
Duas causas, nenhuma no nosso DSP:

1. **Janela curta demais.** O primeiro atraso deste reverb é `MADRS[2] = 11263`
   amostras — **255 ms**. A captura tinha 4000 amostras (90 ms). Nossa saída era
   zero porque o anel ainda não tinha dado a volta; a da referência não era,
   porque o anel dela já vinha cheio. A janela não media o reverb, media o
   silêncio antes dele.
2. **Captura incoerente.** `dsp_program.txt` era gravado quando o programa
   mudava, e `dsp_state.txt` na primeira amostra com sinal — **instantes
   diferentes**. O driver recarrega o programa do DSP durante o boot: o arquivo
   dizia `rbp 11`, o que rodava era `rbp 24`. Comparar os dois é perseguir um bug
   de matemática que não existe.

Corrigidos os dois — captura de programa, estado e RAM no mesmo instante, mais
60.000 amostras — o resultado é **99,7% de amostras idênticas em 60.000, pico
4004 igual ao da referência, média igual, correlação 1,000000**. As 202 amostras
que sobram vêm em rajadas curtas, onde a RAM de som da referência é escrita por
fora da cópia congelada que o harness usa.

Também caiu por terra uma conclusão intermediária: a saída da referência
correlaciona +0,98 com a entrada em atraso zero, e eu li isso como "existe
caminho direto". Não existe — `COEF[11] = 0` corta esse ramo. O que a correlação
mostrava era a **autocorrelação de um tom sustentado** de período ~50 amostras.
Correlação não distingue atraso em sinal periódico; só o trace passo a passo
distingue.

### Os dois defeitos reais no DSP

- **`last_step`.** O hardware roda até o último passo de microcódigo não-zero e
  para (108 passos neste programa, não 128). Os passos vazios não são inócuos:
  cada um ainda multiplica e acumula, e a amostra termina com outro `shift_reg`.
- **`io_addr` é recalculado em todo passo**, inclusive nos que não tocam memória.
  O pendente de leitura do passo anterior é servido antes desse recálculo, então
  o comportamento não muda — mas o endereço que o chip expõe, sim, e sem isso o
  trace passo a passo não fecha.

Com os dois, o trace por passo dá **0 divergentes em 108 passos** nas quatro
primeiras amostras: `shift_reg`, `io_addr`, `read_value` e `inputs` idênticos.

### O nível da interrupção não é fixo: SCILV0/1/2

Nós levantávamos qualquer fonte pendente e habilitada em **nível 4**. O nível
vem, por fonte, de três registradores: o bit `n` de `SCILV0/1/2` forma o nível de
3 bits da fonte `n`, e fonte com os três bits zerados tem nível 0 e **não
interrompe**. Fontes acima do bit 7 usam o nível do bit 7.

O driver da BIOS depende disso: dá **nível 2 ao timer B** (`SCILV1 = 0x0080`) e
deixa o **timer A em 0**, servindo o A por varredura no laço principal. Com tudo
em nível 4 o 68000 entrava pelo autovetor errado. Corrigido, o driver salta de
2,59 M para 11,08 M instruções e passa a realimentar TIMA **e** TIMB — o mesmo
padrão da referência.

### O stub da campainha saiu

`SoundRam` zerava o byte de comando em toda escrita que o cobrisse, fazendo o
papel de um driver que não existia. Com o 68000 real isso é um defeito: a BIOS lê
o zero como "consumido" para comandos que o driver nunca viu. Quem zera é o
68000 — e a referência confirma: ele varre e limpa 0x700–0x73A.

### A cadeia de mixagem, agora como o hardware faz

Estava em ponto flutuante, com o envio ao DSP tirado do registrador errado e o
retorno entrando a nível cheio. Agora é inteira e na ordem certa:

- **seco**: `output >> sdl_shift(DISDL)`, depois o pan, depois `>> 1`;
- **envio ao DSP**: `output >> sdl_shift(IMXL)`, `<< 4` (barramento de 20 bits),
  no canal que **ISEL** nomeia — não EFSDL, que é outra coisa;
- **retorno**: `EFREG[i] >> sdl_shift(EFSDL do slot i)`, pan por EFPAN, `>> 1`;
- **mestre**: `>> (0xF - MVOL)`.

`sdl_shift(0)` é silêncio, `sdl_shift(7)` é unidade — atenuação em deslocamentos,
não razão. O pico da saída caiu de **32767 (saturando) para 7528**.

### O que ainda falta: o gerador de envelope

Com tudo acima, o toque de boot sai, mas **se arrasta por 8,5 s** em vez de
decair. É a lacuna declarada no topo deste arquivo: temos TL estático onde o
hardware tem quatro fases (ataque, decaimento 1, decaimento 2, liberação) com
taxas derivadas de KRS, OCT e FNS. O driver não toca nos registradores do slot
entre 0,634 s e 8,779 s — quem devia baixar o volume é o envelope, e ele não
existe aqui. É o próximo item, e é trabalho conhecido, não descoberta.

O caminho honesto para ele segue a regra do projeto: **capturar o dado, não o
código**. A instrumentação já grava estado do DSP; o mesmo padrão serve para
despejar a atenuação do slot amostra a amostra e validar o nosso gerador contra
ela.

## Como saber que o som saiu certo

Em dois níveis, e nessa ordem:

1. **Evento — as escritas nos registradores do SCSP.** Se o driver programa os
   mesmos slots com os mesmos valores, na mesma ordem, o som está certo por
   construção, e uma divergência aponta o registrador exato. É o análogo do
   `--trace-check`.
2. **Onda — o PCM.** Alinhamento por correlação e erro médio por banda, como o
   erro de pixel faz no vídeo. Serve de piso no gate, não de diagnóstico:
   dois emuladores nunca geram amostras idênticas, e um sample de diferença no
   ataque muda o arquivo inteiro sem dizer o que quebrou.

Os dois lados vêm do emulador instrumentado (`tools/trace-capture/`), que ainda
precisa ganhar: despejo do PCM, trace das escritas no SCSP e trace de PCs do
68000.

## O desenho da thread de som

Áudio vai para uma thread própria — mas a sincronização é de **mão única**, que
é o que evita a classe de bug que trava emulador e estraga FMV (o encontro de
duas vias, em que a CPU espera o som e o som espera a CPU).

- **A RAM de som pertence à thread de som.** As escritas da SH-2 chegam como
  mensagens carimbadas com o ciclo, por fila SPSC sem trava.
- **Os poucos bytes que a SH-2 lê de volta** (a caixa de correio) saem de um
  retrato atômico publicado pela thread de som.
- **O contrato de tempo é um número:** a CPU publica o ciclo emulado; o som
  processa até ele e **nunca além**. O som pode atrasar; adiantar seria ler
  memória que a SH-2 ainda não escreveu.
- **A CPU nunca espera o som.** Só um lado pode bloquear, então não há ciclo de
  espera e o impasse é estruturalmente impossível.
- Saída de amostras por anel SPSC para o consumidor — hoje um arquivo, sem
  prazo nenhum.

Invalidação de código vem de graça nesse desenho: o driver é carregado na RAM
pela SH-2, e como as escritas chegam como mensagens à dona da memória, a
invalidação acontece em série, numa thread só.

## Ordem de trabalho proposta

Feito em 2026-09-20: a captura do DSP (programa, estado, RAM, entrada/saída e
trace por passo) está em `tools/trace-capture/`; os temporizadores e o nível de
interrupção do SCSP estão certos; a caixa de correio real substituiu o stub; a
cadeia de mixagem é a do hardware. O que resta:

1. **Gerador de envelope** — as quatro fases com as taxas de KRS/OCT/FNS, no
   lugar do TL estático. É o que falta para o toque decair em vez de se arrastar
   por 8,5 s.
2. Estender a captura ao PCM e aos PCs do 68000, para medir onda e evento.
3. Mixar em blocos, pulando slot silencioso, se o perfil pedir.
4. Só então medir se alguma coisa precisa de JIT.

Um efeito colateral esperado: hoje a numeração de quadro corre adiantada porque
encurtamos justamente as esperas do driver de som. Com o driver real, essas
esperas voltam a custar o que custam.
