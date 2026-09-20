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

1. Estender a captura: PCM, escritas no SCSP e PCs do 68000.
2. Ligar o 68000 ao restante: temporizadores do SCSP, a interrupção de pedido de
   som para a SH-2, e a caixa de correio real no lugar do stub que a limpa.
3. SCSP: slots PCM com envelope, volume e pan, mixando em blocos, pulando slot
   silencioso.
4. Só então medir se alguma coisa precisa de JIT.

Um efeito colateral esperado: hoje a numeração de quadro corre adiantada porque
encurtamos justamente as esperas do driver de som. Com o driver real, essas
esperas voltam a custar o que custam.
