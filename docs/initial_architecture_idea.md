# Primeiro esboço com gemini

Para implementar essa arquitetura de atores isolados com um Mediator centralizado e buffers de memória compartilhada em Rust, você tem excelentes opções tanto na biblioteca padrão (`std`) quanto no ecossistema de crates assíncronas (como o ecossistema `tokio`).

---

## 1. Ferramentas no Core (`std`) e no Ecossistema Principal

Para a parte de **mensageria e isolamento**:

* **`tokio::sync::mpsc` e `oneshot**`: Como vimos, são os canais assíncronos ideais para o envio de mensagens do Mediator para os processos e para o retorno assíncrono (*Response Channels*).
* **`std::sync::mpsc`**: O canal nativo do Rust. É síncrono e excelente se você estiver trabalhando com threads nativas do sistema operacional (`std::thread`) em vez de tarefas assíncronas.

Para a parte de **memória compartilhada (Shared Memory Buffers)**:
Se os processos precisam acessar buffers de dados comuns de forma extremamente rápida, o Rust oferece primitivas seguras para compartilhar memória sem precisar duplicar dados:

* **`std::sync::Arc<T>` (Atomic Reference Counted):** Permite que múltiplos processos (threads/tasks) tenham um ponteiro de leitura para o mesmo bloco de memória. O dado fica em uma região de memória imutável e segura.
* **`tokio::sync::RwLock<T>` ou `std::sync::RwLock<T>` (Read-Write Lock):** Ideal para buffers. Permite que **infinitos processos leiam** o buffer ao mesmo tempo, mas se o Orchestrator precisar atualizar o buffer, ele ganha acesso exclusivo de escrita, bloqueando temporariamente as leituras.

---

## 2. Como Integrar Memória Compartilhada na Arquitetura

Em vez de passar grandes volumes de dados (como um arquivo ou um payload pesado de telemetria) dentro das mensagens do Mediator, você coloca esses dados em um buffer protegido por um `Arc<RwLock<Buffer>>`.

Nas mensagens do Mediator, você envia apenas o **ID ou o metadado** do que precisa ser lido. O processo isolado vai até o buffer e busca o dado sob demanda.

### Exemplo Prático em Rust (Tokio + Arc + RwLock)

```rust
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use std::collections::HashMap;

// 1. O Buffer de Memória Compartilhada
#[derive(Default)]
pub struct SharedBuffer {
    // Simulando um cache de pacotes ou payloads indexados por ID
    pub storage: HashMap<u32, Vec<u8>>,
}

// 2. A Mensagem do Mediator agora é leve: só carrega a referência (ID) do dado
#[derive(Debug)]
pub struct Envelope {
    pub data_id: u32,
    pub tx_respond: oneshot::Sender<String>,
}

// 3. O Processo Isolado que busca dados no Buffer
pub struct Worker {
    rx: mpsc::Receiver<Envelope>,
    // Cada worker ganha uma cópia do ponteiro Arc para acessar a mesma memória
    shared_buffer: Arc<RwLock<SharedBuffer>>,
}

impl Worker {
    pub fn new(rx: mpsc::Receiver<Envelope>, shared_buffer: Arc<RwLock<SharedBuffer>>) -> Self {
        Self { rx, shared_buffer }
    }

    pub async fn run(mut self) {
        while let Some(envelope) = self.rx.recv().await {
            // Adquire o Lock de Leitura (Múltiplos workers podem ler simultaneamente)
            let buffer_guard = self.shared_buffer.read().await;
            
            // Busca o dado bufferizado de forma extremamente rápida (In-Memory)
            let resposta = if let Some(raw_bytes) = buffer_guard.storage.get(&envelope.data_id) {
                format!("Dado processado com sucesso. Tamanho: {} bytes", raw_bytes.len())
            } else {
                "Erro: Dado não encontrado no buffer compartilhado".to_string()
            };

            // Drop explícito do guard para liberar a leitura o quanto antes (opcional, ocorre no fim do escopo)
            drop(buffer_guard); 

            let _ = envelope.tx_respond.send(resposta);
        }
    }
}

#[tokio::main]
async fn main() {
    // Criando o buffer na Heap e envolvendo-o em um Arc de leitura distribuída
    let shared_buffer = Arc::new(RwLock::new(SharedBuffer::default()));

    // --- Simulação de Escrita (ex: O Orchestrator ou Driver de Entrada populando o buffer) ---
    {
        let mut write_guard = shared_buffer.write().await;
        write_guard.storage.insert(1001, vec![0xDE, 0xAD, 0xBE, 0xEF]); // Dados brutos bufferizados
        println!("[Buffer] Dados populados na memória compartilhada.");
    } // O Lock de escrita é liberado aqui automaticamente

    // Configurando o Worker
    let (tx_worker, rx_worker) = mpsc::channel::<Envelope>(10);
    // Clonar o Arc apenas incrementa o contador de referências, não duplica os dados na memória!
    let worker = Worker::new(rx_worker, shared_buffer.clone()); 
    tokio::spawn(worker.run());

    // Enviando requisição via Mediator para o Worker processar o ID 1001
    let (tx_respond, rx_respond) = oneshot::channel();
    let msg = Envelope { data_id: 1001, tx_respond };
    
    tx_worker.send(msg).await.unwrap();

    if let Ok(res) = rx_respond.await {
        println!("[Resultado]: {}", res);
    }
}

```

### Bibliotecas de Terceiros Prontas (Alternativas Avançadas)

Se você não quiser montar essa infraestrutura de canais na mão, existem crates consolidadas no ecossistema Rust para isso:

1. **Para o Modelo de Atores puro:**
* **`actix`**: É a biblioteca mais famosa de atores em Rust. Ela resolve todo o gerenciamento de ciclo de vida de processos isolados e troca de mensagens sob o padrão Mediator nativamente.


2. **Para Buffers de Memória Compartilhada de altíssima performance (Zero-Copy):**
* **`bytes`**: Fornece a struct `Bytes`, que é um buffer de memória gerenciado por contador de referências de forma eficiente, perfeito para tráfego de rede ou IO de disco sem alocações extras.
* **`shared_memory` / `iceoryx2**`: Se esses processos fossem processos reais do Sistema Operacional (com PIDs diferentes no Linux) e não threads/tasks do mesmo programa, essas crates permitem criar memória compartilhada IPC (*Inter-Process Communication*) real via SHM do kernel.


# Segunda sessão com Claude

pode ser visto em first_architectural_draft.md


# Minha ideia inicial sobre a arquitetura

* Primeira premissa era usar processos isolados e não threads, as threads deveriam ser agnosticas e se comunicar através do mediator
* a trava entre os processos não era para ser um mutex e sim blocos de shared memory, aonde o processo iria buscar a informação exatamente como ocorre no saturn, as soluções arquiteturais do proprio saturn são performaticas, pois os processos são independentes, eles fazem o que tem de fazer sem ligar para um estado global, pois esse estado é compartilhado através de buffers, dma e memoria compartilhada, cujo quem controla se solta, se limpa, se atualiza é o jogo em si através de instruções
* na primeira sessão com o gemini, vemos claramente que a lib a ser usada era uma voltada a processos e não threads.
* É evidente a arquitetura saturn que quem controla as coisas é o game e não o hardware, minha ideia é que cada processo rode como uma parte do hardware e aguarde a orquestração do game e não da nossa implementação, a implementação é burra digamos assim, ela recebe a instrução do jogo e executa. Ela faz isso usando os buffers e o mediator central que distribui os eventos