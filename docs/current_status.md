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

# 2. Desconfiança de otimização (palpite, não medido isoladamente)

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

# 3. Desconfiança do erro de som (palpite, não medido)

Sintoma relatado: **tonalidade e velocidade estão certas, mas o som fica em loop,
sem cauda e sem o "shuuuan"**.

Isso é consistente com o que já está medido e documentado em `docs/sound.md`, e
a minha principal desconfiança é **uma coisa só**:

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

Está medido que **há uma única chave de slot em todo o boot** (`1 key-ons`). Se a
máquina real liga mais de um slot, o "shuuuan" pode ser um **som próprio**, e aí
o envelope resolve o loop e a cauda mas não ele.

Não sei qual das duas é, e **não vou adivinhar**: as duas previsões são
distinguíveis por medição, e a medição é barata.

## Como decidir entre as duas, sem chutar

A ordem que eu seguiria, e o que cada passo responde:

1. **Contar as chaves de slot na referência.** A instrumentação de
   `tools/trace-capture/` já grava eventos de chave. Se a referência liga um
   slot só, a desconfiança secundária morre e sobra o envelope. Se liga dois, o
   "shuuuan" tem dono e é outro trabalho. **É o passo mais barato e o que mais
   separa os caminhos — faça este primeiro.**
2. **Capturar a atenuação do slot, amostra a amostra**, pelo mesmo padrão que já
   usamos para o DSP: dado, não código. Isso vira o oráculo do envelope, do jeito
   que `dsp_check` virou o oráculo do DSP.
3. **Implementar as quatro fases** e validar contra essa captura, em vez de
   validar de ouvido.

Vale lembrar a lição que esta sessão já cobrou caro: **a captura tem de ser
coerente**. Programa gravado num instante e estado em outro custou horas
caçando um bug de matemática que não existia. Se a captura do envelope não sair
do mesmo instante que o resto, o mesmo erro se repete de outra forma.

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
