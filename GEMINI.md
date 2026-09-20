# GEMINI.md

O guia completo é o [`CLAUDE.md`](CLAUDE.md) — leia-o. Este arquivo existe para
o que muda quando quem trabalha é o Gemini.

## As regras, em resumo

1. Só vídeo, e o que o vídeo depende, é real. O resto são stubs.
2. Só JIT. Sem interpretador, sem volta para interpretador por opcode.
3. OpenGL puro e headless (EGL surfaceless + `gl`). Nada de glium, winit, glutin.
4. **Nunca abra `../yabause`, `../yabassanshiro` ou qualquer emulador vizinho.**
   Tudo sai dos traces, das capturas e da BIOS.
5. OOP e SOLID: a CPU só conhece `Sh2Bus`; chips implementam `MemoryDevice`.
6. **`archived/` está fora dos limites.** É o projeto antigo; não leia, não
   copie, não dependa.
7. Não toque em `tools/`. É o instrumento de medição; editá-lo enquanto se mede
   é medir a si mesmo. Se achar que uma ferramenta está errada, **pare e
   reporte**, não conserte.
8. Não cale uma verificação: nada de `assert!(true)`, `let _ =` no lugar de
   asserção, `#![allow(...)]`, `cargo clippy --fix`, nem reescrever código para
   o verificador parar de casar.
9. Limiar só aperta. Afrouxar exige `MIMAS_OVERRIDE_REASON`, e o verde sai
   marcado.

## Você é quem commita

O Claude não commita neste repositório; você sim. Por isso esta verificação é
sua e tem de rodar **antes** do commit, não depois:

```bash
MIMAS_COVERAGE_COMMITS=HEAD bash tools/quality_gate.sh
```

`HEAD` como base significa "diferença entre o HEAD e a árvore de trabalho", que
antes do commit é exatamente a mudança que você vai commitar. Depois do commit o
`HEAD` andou e o mesmo comando não mede mais nada — não há como rodar essa
verificação retroativamente, e é por isso que a ordem importa.

Se vier vermelho, a saída nomeia as linhas sem cobertura. Escreva testes para
elas. Não baixe o piso, não alargue o intervalo até o número melhorar, e não
commite vermelho para consertar depois.

## No relatório

- os dois resumos do gate, colados, não parafraseados;
- o erro médio do vídeo e a porcentagem do trace, antes e depois;
- cada arquivo que você mudou e por quê;
- o que achar errado em `tools/`, reportado, não consertado.

"Destravei X" só conta se a medição andou. Se você não mediu, não aconteceu.
