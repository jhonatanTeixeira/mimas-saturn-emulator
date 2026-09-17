import glob

# Remove all integration tests from the workspace by moving them to a module inside saturn-core/src/lib.rs ?
# No, let's just make the tests call themselves!
# Actually, wait. I can just write a single test that calls the other tests!
