# O quality gate

```bash
bash tools/quality_gate.sh
```

Onze passos determinísticos: sem modelo, sem rede, sem aleatoriedade. Roda todos
e reporta no fim — um vermelho não interrompe os outros, porque saber que três
coisas quebraram vale mais do que saber qual quebrou primeiro.

Ele não é formalidade. Num projeto irmão, dois agentes num único dia quebraram o
boot inteiro, deixaram um `is_shutdown()` retornando `false` para sempre,
reintroduziram uma syscall por instrução emulada que tinha sido medida e
removida naquela mesma manhã, e entregaram quatro testes `assert!(true)` cujos
nomes afirmavam verificar a troca de framebuffer. `cargo build`, `cargo test`
(396 verdes) e `cargo clippy` ficaram satisfeitos o tempo todo.

## Os passos

| # | passo | o que mede | única correção honesta |
|---|---|---|---|
| 1 | formatação | `cargo fmt --check` | `cargo fmt` |
| 2 | clippy | número de avisos contra um teto | corrigir o aviso. **Nunca `cargo clippy --fix`**: ele já transformou uma guarda de opcode em no-op enquanto calava o aviso que apontava o problema |
| 3 | testes | `cargo test` | consertar o código ou o teste |
| 4 | testes que não afirmam nada | `#[test]` sem asserção, ou afirmando constante | escrever a asserção, ou `// no-assert: <motivo>` no corpo do teste |
| 5 | regras do projeto | vocabulário do Yabause, dependência de `archived/`, camada de janela | remover a violação; não há allowlist |
| 6 | cobertura do diff | % das linhas que **você** mudou cobertas por teste | escrever o teste. Não `--exclude-files`, não baixar o piso |
| 7 | vídeo | erro médio de pixel contra as capturas reais | se a imagem piorou, é regressão; se melhorou, baixe o teto |
| 8 | trace | % dos PCs do trace real que executamos, e opcodes divergentes | se caiu, é regressão; se subiu, suba o piso |
| 9 | áudio | correlação do envelope de loudness (RMS por janela de 50 ms) contra `stubs/captures/audio/boot.wav`, um PCM de 12 s gravado de um console real via loopback (ver `docs/sound.md`) | se caiu, é regressão; se subiu, suba o piso. Não é diagnóstico — um sample de diferença no ataque muda a correlação inteira sem dizer o que quebrou, igual à ressalva já feita sobre a captura do DSP |
| 10 | vídeo do jogo atual | quadros de `stubs/captures-game/frames/` contra o que o mimasv2 gera para o mesmo jogo | **pulado hoje**: sem referência gravada, ou o mimasv2 ainda não roda jogo (CD Block é stub). Trabalhamos com um jogo por vez — a pasta não tem nome de jogo no caminho, é sempre "o jogo atual" |
| 11 | áudio do jogo atual | `stubs/captures-game/audio.wav` contra o que o mimasv2 gera, mesmo método do passo 9 | **pulado hoje**, mesmo motivo do passo 10 |

Os passos 7, 8 e 9 são os que medem o projeto de verdade hoje. Os passos 10 e
11 vão se juntar a eles quando o mimasv2 rodar um jogo de verdade — a
infraestrutura já existe, só falta o que comparar do nosso lado.

## Limiares

Todos medidos neste repositório, não copiados de lugar nenhum. **Apertar é
livre; afrouxar falha o gate** a menos que `MIMAS_OVERRIDE_REASON` diga por quê,
e o resumo marca que aquele verde saiu sob limiar afrouxado.

| variável | padrão | direção |
|---|---|---|
| `MIMAS_MAX_WARNINGS` | 25 | só desce |
| `MIMAS_MAX_MEAN_ERR` | 1.66 | só desce |
| `MIMAS_MIN_TRACE_PCT` | 92.1 | só sobe |
| `MIMAS_MIN_DIFF_COV` | 90 | só sobe |
| `MIMAS_MIN_AUDIO_CORR` | 0.707 | só sobe |
| `MIMAS_COVERAGE_COMMITS` | `HEAD` | base do diff de cobertura |
| `MIMAS_FRAMES` | 620 | quadros gerados na medição de vídeo |
| `MIMAS_AUDIO_FRAMES` | 750 | quadros gerados na medição de áudio (~12 s, cobre a janela da captura) |

Quando um número melhora, o gate imprime o novo valor e pede para apertar o
limiar. Apertar é como o ganho deixa de poder se perder em silêncio.

## Cobertura: só do que mudou

A árvore inteira tem cobertura baixa, e medi-la a cada rodada não ajudaria
ninguém — o denominador é inflado pela expansão das macros do dynasm. O gate
mede **as linhas que a sua mudança toca**, com o mesmo piso de 90%.

`MIMAS_COVERAGE_COMMITS=HEAD` significa "diferença entre o HEAD e a árvore de
trabalho", isto é, o trabalho ainda não commitado. Duas coisas que a ferramenta
se recusa a responder errado, e que não devem ser contornadas:

- **Intervalo cujo fim não é o HEAD.** A cobertura é medida contra a árvore de
  trabalho, a única versão cujos números de linha podem ser cruzados com os
  dados.
- **Dados de cobertura mais velhos que os fontes.** Se um `.rs` mudou depois do
  LCOV, os números descrevem outra versão do código. Rode o tarpaulin de novo;
  não passe `--allow-stale` para a mensagem sumir.

Uma mudança que só toca testes ou documentação passa com "nothing to measure":
`--ignore-tests` mantém o corpo dos testes fora do denominador, então escrever
teste aparece como cobertura do código que ele exercita, nunca dele mesmo.

## Ao reportar

Diga o que aconteceu. "9 de 11 verdes, 2 pulados, formatação vermelha" é útil.
"O gate passou", quando um limiar foi afrouxado ou um passo pulado, não é. Um passo
pulado não é um passo verde, e o resumo do gate diz isso em voz alta.
