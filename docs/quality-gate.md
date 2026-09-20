# O quality gate

```bash
bash tools/quality_gate.sh
```

Oito passos determinísticos: sem modelo, sem rede, sem aleatoriedade. Roda todos
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

Os passos 7 e 8 são os que medem o projeto de verdade. Os outros seis impedem
que o caminho até eles apodreça.

## Limiares

Todos medidos neste repositório, não copiados de lugar nenhum. **Apertar é
livre; afrouxar falha o gate** a menos que `MIMAS_OVERRIDE_REASON` diga por quê,
e o resumo marca que aquele verde saiu sob limiar afrouxado.

| variável | padrão | direção |
|---|---|---|
| `MIMAS_MAX_WARNINGS` | 35 | só desce |
| `MIMAS_MAX_MEAN_ERR` | 1.66 | só desce |
| `MIMAS_MIN_TRACE_PCT` | 92.1 | só sobe |
| `MIMAS_MIN_DIFF_COV` | 90 | só sobe |
| `MIMAS_COVERAGE_COMMITS` | `HEAD` | base do diff de cobertura |
| `MIMAS_FRAMES` | 620 | quadros gerados na medição de vídeo |

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

Diga o que aconteceu. "7 de 8 verdes, formatação vermelha" é útil. "O gate
passou", quando um limiar foi afrouxado ou um passo pulado, não é. Um passo
pulado não é um passo verde, e o resumo do gate diz isso em voz alta.
