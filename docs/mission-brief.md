# Missão: Interpretador JIT para a BIOS do Sega Saturn

## Objetivo
A missão principal deste projeto é desenvolver um interpretador JIT (Just-In-Time) em **Rust** que seja capaz de executar a BIOS do Sega Saturn (`saturn_bios.bin`). O sistema irá interpretar o código de máquina do processador principal (SH-2) e rotear adequadamente as chamadas para os diferentes subsistemas do console.

## Arquivos de Base e Traces
O repositório já contém a BIOS do Sega Saturn, além de 4 arquivos com traces reais de execução capturados a partir de outro emulador funcional. Estes arquivos são a base da nossa engenharia reversa e validação:
- `saturn_bios.bin`: O arquivo binário da BIOS original.
- `bios_trace_no_game.txt`: Trace completo da execução da BIOS sem inserção de jogo.
- `bios_trace_with_game.txt`: Trace completo da execução da BIOS com um jogo carregado.
- `branch_trace_no_game.txt`: Trace contendo apenas os saltos (branches) sem jogo.
- `branch_trace_with_game.txt`: Trace contendo apenas os saltos (branches) com jogo.

### Análise dos Traces e Mapeamento de Hardware
As investigações dos traces `bios_trace_*` revelam exatamente como o código da CPU (Core: M - Master) interage com os chips e memórias do sistema. Observamos estruturas como:
- `Opcode: 2102 | ... | Mem: WRITE 25FE00C4 (VDP2 Regs)`
- `Opcode: B027 | ... | Mem: WRITE 2010001F (SMPC)`
- `Opcode: 2342 | ... | Mem: WRITE 06000210 (Work RAM High)`
- `Opcode: 2342 | ... | Mem: WRITE 00200000 (Work RAM Low)`

Estes acessos demonstram a necessidade de interceptar leituras e escritas de memória (Memory Mapped I/O) e delegá-las aos componentes corretos do console.

## Arquitetura e Princípios (OOP e SOLID)
Para garantir um código escalável, limpo e de fácil manutenção, o desenvolvimento do JIT em Rust deve aplicar estritamente os conceitos de **Programação Orientada a Objetos (OOP)** adaptados para o ecossistema do Rust, bem como os **Princípios SOLID**.

### Diretrizes Arquiteturais:
1. **Responsabilidade Única (SRP)**:
   - O interpretador JIT deve se preocupar **exclusivamente** com o fetch, decode e a execução lógica/aritmética das instruções (Opcodes) da CPU SH-2.
   - Operações de leitura e escrita em memória não devem ser processadas internamente pela CPU. Elas devem ser repassadas a um gerenciador de barramento (Memory Bus).

2. **Abstração de Componentes (Chips Separados)**:
   - Cada chip e área de memória do Saturn deverá possuir a sua própria classe/struct separada e isolada. 
   - Baseado nos traces, precisaremos inicialmente de abstrações independentes para:
     - **VDP1 / VDP2** (Gráficos)
     - **SMPC** (System Manager & Peripheral Control)
     - **SCU** (System Control Unit)
     - **Work RAM High / Work RAM Low**
     - **ROM/BIOS**

3. **Inversão de Dependência e Segregação de Interfaces (DIP / ISP / LSP)**:
   - Os chips não devem ser acoplados diretamente ao JIT.
   - Criaremos Traits em Rust (ex: `MemoryDevice`) com assinaturas de métodos para leitura e escrita (8-bit, 16-bit, 32-bit). Cada componente/chip (SMPC, VDP2, etc.) irá implementar essa trait.
   - A CPU JIT conversará com um `MemoryBus`, e este Bus distribuirá as chamadas polimorficamente para a classe/struct correspondente usando o endereço de memória como chave de roteamento, espelhando fielmente o comportamento dos traces.

4. **Aberto para Extensão (OCP)**:
   - O barramento de memória (Bus) deve permitir plugar novos chips ou expansões de memória apenas registrando a nova implementação que obedece à interface, sem modificar as classes já existentes ou a própria CPU.

## Próximos Passos
1. Estruturar a modelagem base e as Traits de comunicação de I/O.
2. Construir e registrar as structs falsas (mock/stub) ou iniciais para `VDP2`, `SMPC`, `WorkRAM`, etc., mapeando seus respectivos ranges de endereço.
3. Desenvolver o core do SH-2 e a estrutura básica de fetch/decode do interpretador JIT.
4. Validar o fluxo inicial do JIT carregando o `saturn_bios.bin`, executando as primeiras instruções e batendo o log de saída gerado com os traces fornecidos.
