with open("saturn-core/src/sync.rs", "r") as f:
    text = f.read()
text = text.replace("pub struct LockStepSync {\n    num_threads: usize,\n    slack_limit: u64,", 
                    "pub struct LockStepSync {\n    shutdown_flag: std::sync::atomic::AtomicBool,\n    num_threads: usize,\n    slack_limit: u64,")
text = text.replace("Self {\n            num_threads,\n            slack_limit,",
                    "Self {\n            shutdown_flag: std::sync::atomic::AtomicBool::new(false),\n            num_threads,\n            slack_limit,")
with open("saturn-core/src/sync.rs", "w") as f:
    f.write(text)
