# Mimas v2 — emulador de Sega Saturn em Rust, escrito por IA

Todo o código deste repositório foi escrito por IA. A pesquisa, a arquitetura e
a direção de engenharia — o que construir, em que ordem, que compromisso
aceitar, e como conferir uma correção contra o comportamento real do hardware em
vez de confiar num teste auto-consistente — vêm de um engenheiro humano
conduzindo o processo.

## O que é

O objetivo é estreito e mensurável: **bootar a BIOS real do Saturn sobre um JIT
x86-64 e reproduzir a saída de vídeo dela, sem tela, quadro a quadro, contra
capturas reais**.

O método é o que diferencia este projeto:

- **A verdade vem da execução real**, não de outro emulador. Traces de execução
  da BIOS (primeira execução de cada endereço, com os valores lidos) e capturas
  de tela do console são a referência. Nenhum emulador vizinho é consultado:
  trace é comportamento sem implementação, e por isso não há de onde copiar
  arquitetura.
- **Todo avanço é medido**: erro médio de pixel contra as capturas e
  porcentagem do trace real reproduzida. Um `tools/quality_gate.sh` de 8 passos
  determinísticos guarda os dois pisos, junto com formatação, avisos, testes e
  cobertura das linhas alteradas.
- **Só vídeo é real**; o resto são stubs que devolvem exatamente o que a máquina
  real respondeu naquele ponto do trace, cada um com a procedência anotada.

Estado atual, lacunas e números: [`docs/status.md`](docs/status.md).

## Como rodar

As capturas de referência estão no repositório (`stubs/captures/`). A BIOS e os
traces não: eles são o programa da BIOS. Para produzir os seus traces a partir
de uma BIOS que você já tenha, há um patch de instrumentação e as instruções em
[`tools/trace-capture/`](tools/trace-capture/). Com os dois em disco:

```bash
cargo build --release
./target/release/mimasv2 --frames 620 --dump out
./target/release/compare stubs/captures out --max-frame 728
```

## Histórico

Este repositório começou como **mimas**, um emulador de Saturn com uma thread
por chip. Aquela árvore está em [`archived/`](archived/), preservada com o
histórico do git e fora dos limites dos agentes. O que se aprendeu lá, e por que
o projeto recomeçou, está em [`docs/status.md`](docs/status.md) e no
[`CLAUDE.md`](CLAUDE.md).
