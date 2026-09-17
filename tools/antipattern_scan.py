#!/usr/bin/env python3
"""Find code that resembles a known anti-pattern, by semantic similarity.

Deliberately **not** part of `tools/quality_gate.sh`. The gate is stdlib-only so
that it always runs; this needs torch, transformers and two models. It is run on
demand, and its output is a review queue, never a verdict.

Why it exists next to `tools/golden_rules.py` rather than instead of it: those
rules are hand-written, so they catch exactly the eight shapes that were already
found by hand. A rule list only knows yesterday's bug. Of the exemplars in
`antipatterns/corpus.json`, **five have no deterministic rule at all** -- a
tautological assertion, an opcode mask that can never match, `std::env::var` on
the memory hot path, a `SLEEP` that busy-waits, a global mutex reached through a
helper. Those are the ones this is for.

Pipeline:

    corpus of "what not to do"  (pointers into git history, with provenance)
      -> GraphCodeBERT embeds every function in the tree
      -> top-K nearest neighbours of each exemplar
      -> a small LLM answers one narrow question per candidate:
         "is this the same defect as the exemplar, yes or no?"
      -> review queue

Each stage does what it is good at. The embedding model does code similarity,
which is what it was trained for; the LLM never sees "audit this architecture",
only a short snippet and a concrete exemplar to compare it against.

**Measure before trusting.** `--gold` replays the pipeline against the commits
where these defects existed and reports precision and recall. Until that has been
run and read, this tool's output is a suggestion. Numbers first, promotion later
-- the same discipline the coverage and mutation steps are held to.

**Determinism** is achievable here but has to be deliberate: model revisions and
the quantization config are pinned below, generation is greedy, and the
similarity threshold is versioned alongside the corpus. All four are part of the
contract -- changing the quantization alone can flip a verdict on the same
snippet. Without that the tool answers differently for the same input, which is
the defect this whole toolchain exists to remove.

Everything runs on CPU by design. A tool that needs a particular GPU is a tool
that is unavailable on the machine that happens to need it.
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rustscan import function_bodies, production_code, rust_files

HERE = Path(__file__).resolve().parent
CORPUS = HERE / "antipatterns" / "corpus.json"

# Model cache lives next to the repo, not in ~/.cache.
#
# Set here rather than left to the environment, because the failure it prevents
# is silent and expensive: on this machine `/` (which holds $HOME) runs at 93%
# with under 7 GB free, while the volume the repo sits on has 110 GB. A default
# `~/.cache/huggingface` fills the small disk and takes the rest of the system
# down with it. Setting it in the shell would work until someone ran the tool
# from a shell that had not.
#
# Must happen before transformers or huggingface_hub is imported -- both read
# these variables at import time.
MODEL_CACHE = HERE.parent / ".models"
os.environ.setdefault("HF_HOME", str(MODEL_CACHE))
os.environ.setdefault("HF_HUB_CACHE", str(MODEL_CACHE / "hub"))

# The Python packages live in a project-local venv, for the same reason: torch
# and its dependencies are ~1.2 GB, and site-packages under $HOME is on the disk
# that is already nearly full. Run this tool with `.venv/bin/python`.
VENV_DIR = HERE.parent / ".venv"
VENV_PY = VENV_DIR / "bin" / "python"

# Pinned so two runs on the same tree give the same answer.
#
# Both chosen to run on CPU, which is the point: this tool has to work on any
# machine that has the repo, not only one with a GPU. GraphCodeBERT is 125M
# parameters and embeds this tree's ~630 functions in seconds; Qwen3.5-0.8B is
# small enough to answer a few hundred yes/no questions without one.
#
# Scale-up path, if measured precision is not good enough: Qwen3.5 ships 2B, 4B
# and 9B in the same family, so the pin below is the only line that changes.
MODELS = {
    "embedder": "microsoft/graphcodebert-base",
    "verdict": "Qwen/Qwen3.5-0.8B",
}

# int8 weight-only, through transformers' torchao integration. Weight-only (not
# dynamic-activation) because this workload is memory-bound: a few hundred short
# yes/no completions, where loading weights dominates and activation precision
# buys nothing measurable.
#
# The same config is used on GPU and CPU deliberately. Quantization changes the
# weights the model actually runs, so switching it per device would let the same
# snippet get a different verdict depending on the machine -- which would quietly
# break the determinism this file claims.
#
# Part of the determinism contract, not a performance knob: change the quant
# config and the same snippet can get a different verdict, so it is pinned here
# alongside the model revisions and reported by `gold`.
QUANT = "Int8WeightOnlyConfig"


def pick_device() -> str:
    """CUDA when it is there, CPU otherwise.

    The requirement is that this *runs* on CPU, not that it runs *only* on CPU:
    a tool that needs a particular GPU is unavailable on the machine that happens
    to need it. Both pinned models are sized so CPU is viable -- GraphCodeBERT is
    125M parameters, and Qwen3.5-0.8B at int8 is well under 1 GB. Where a GPU is
    present the same work just finishes sooner.
    """
    import torch

    return "cuda" if torch.cuda.is_available() else "cpu"


def load_verdict_model():
    """Load the verdict model, int8 weight-only, on whichever device is there.

        from transformers import AutoModelForCausalLM, AutoTokenizer, TorchAoConfig
        from torchao.quantization import Int8WeightOnlyConfig

        model = AutoModelForCausalLM.from_pretrained(
            MODELS["verdict"],
            device_map=pick_device(),
            quantization_config=TorchAoConfig(quant_type=Int8WeightOnlyConfig()),
        )

    `generate(..., disable_compile=True)`: torchao compiles on first inference and
    recompiles whenever batch size or `max_new_tokens` changes. Verdicts are one
    short completion each, so compilation would cost more than it saves.
    """
    _require_ml()
    MODEL_CACHE.mkdir(parents=True, exist_ok=True)
    from transformers import AutoModelForCausalLM, AutoTokenizer, TorchAoConfig
    from torchao.quantization import Int8WeightOnlyConfig

    dev = pick_device()
    print(f"  verdict model on {dev} ({QUANT})")
    tok = AutoTokenizer.from_pretrained(MODELS["verdict"])
    model = AutoModelForCausalLM.from_pretrained(
        MODELS["verdict"],
        device_map=dev,
        quantization_config=TorchAoConfig(quant_type=Int8WeightOnlyConfig()),
    )
    return tok, model
# `top_k` and `min_sim` are per exemplar, in the corpus -- not global constants.
# These patterns have very different prevalence, so one K for all of them is a
# guess. The first version used a global top_k=8 with a 0.80 cutoff and produced
# 16 false positives out of 16 reports.
#
# The thresholds there are anchored to a measured distribution, not chosen by
# feel. Cosine between *unrelated* functions in this tree, 160 sampled, all pairs:
#
#     microsoft/graphcodebert-base   p05 0.774   p50 0.878   p95 0.961
#     Qwen/Qwen3-Embedding-0.6B      p05 0.226   p50 0.405   p95 0.642
#
# GraphCodeBERT calls two random Rust functions 88% alike. It was trained on
# CodeSearchNet -- Python, Java, JavaScript, PHP, Ruby, Go -- and Rust is not in
# it, so it compresses the whole repository into a narrow cone and retrieval
# returns "the least indistinguishable" rather than "the most similar". That is
# what put `synthesize` in front of an opcode-decode exemplar, and no amount of
# prompt work downstream would have fixed it.
#
# Against the Qwen distribution, the per-exemplar cutoffs of 0.60-0.70 sit at or
# above p95: "in the top 5% of similarity for this repository". Re-measure and
# re-anchor them if the embedder changes.


@dataclass
class Exemplar:
    id: str
    spec: str
    rev: str
    file: str
    fn: str
    why: str
    caught_by: str | None = None
    fixed_in: str | None = None
    top_k: int = 5
    min_sim: float = 0.65
    checks: list[dict] = field(default_factory=list)
    pattern: str = ""
    lines: list[int] | None = None
    code: str = ""


def load_corpus() -> list[Exemplar]:
    raw = json.loads(CORPUS.read_text())
    return [Exemplar(**{k: v for k, v in e.items()}) for e in raw["exemplars"]]


def resolve(ex: Exemplar) -> tuple[bool, str]:
    """Pull the exemplar's code out of git at its recorded revision.

    Normally the whole function. When `lines` is set, just that slice -- some
    defects are one arm of a match far larger than the embedder's window, and a
    truncated giant carries no signal.
    """
    src = subprocess.run(
        ["git", "show", f"{ex.rev}:{ex.file}"], capture_output=True, text=True
    ).stdout
    if not src:
        return False, f"{ex.rev}:{ex.file} not found"
    if ex.lines:
        a, b = ex.lines
        rows = src.splitlines()
        if b > len(rows):
            return False, f"lines {a}-{b} past end of {ex.rev}:{ex.file}"
        ex.code = "\n".join(rows[a - 1 : b])
        return True, f"{ex.file}:{a}-{b} ({b - a + 1} lines, explicit range)"
    bodies = function_bodies(src, ex.fn)
    if not bodies:
        return False, f"fn `{ex.fn}` not found in {ex.rev}:{ex.file}"
    name, line, body, _ = bodies[0]
    ex.code = body
    return True, f"{ex.file}:{line} ({len(body.splitlines())} lines)"


def cmd_list() -> int:
    """Resolve every exemplar and report. Needs no ML dependencies.

    Worth running on its own: it is what catches an exemplar whose function was
    renamed or whose commit was rewritten, which would otherwise show up as a
    silent hole in the corpus.
    """
    corpus = load_corpus()
    det = sum(1 for e in corpus if e.caught_by)
    print(f"{len(corpus)} exemplars ({det} also covered by a deterministic rule, "
          f"{len(corpus) - det} semantic-only)\n")
    bad = 0
    for ex in corpus:
        ok, detail = resolve(ex)
        mark = "✅" if ok else "❌"
        if not ok:
            bad += 1
        cover = ex.caught_by or "— no deterministic rule"
        print(f"  {mark} {ex.id}")
        print(f"       spec §{ex.spec}  {ex.rev}  {detail}")
        print(f"       covered by: {cover}")
    if bad:
        print(f"\n❌ {bad} exemplar(s) no longer resolve. Fix the pointer in "
              f"{CORPUS.relative_to(Path.cwd()) if CORPUS.is_relative_to(Path.cwd()) else CORPUS}, "
              "do not delete the entry.")
    return 1 if bad else 0


def cmd_chunks() -> int:
    """Count the functions that would be embedded. Needs no ML dependencies.

    GraphCodeBERT takes ~512 tokens, so granularity is one function. This is the
    same chunker the golden-rule checks use, which is why that work was a
    prerequisite for this tool rather than an alternative to it.
    """
    total = oversize = 0
    for p in rust_files(Path(".")):
        prod = production_code(p.read_text(errors="ignore"))
        for _name, _line, body, _at in function_bodies(prod, r"\w+"):
            total += 1
            if len(body) > 4 * 512:  # rough chars-per-token bound
                oversize += 1
    print(f"{total} functions would be embedded")
    print(f"{oversize} exceed ~512 tokens and would be truncated "
          f"({oversize * 100 // max(total, 1)}%)")
    return 0


def _require_ml():
    try:
        import accelerate  # noqa: F401  (transformers needs it for device_map)
        import torch  # noqa: F401
        import torchao  # noqa: F401
        import transformers  # noqa: F401
    except ImportError:
        # `sys.prefix`, not `sys.executable`: a venv's `bin/python` is a symlink
        # to the base interpreter, so resolving the two paths makes them equal
        # and the check silently never fires. `sys.prefix` is what actually
        # distinguishes "running inside this venv" from "running outside it".
        if VENV_PY.exists() and Path(sys.prefix) != VENV_DIR:
            print("❌ Dependencies are installed, but this is not the project venv.")
            print(f"   Run: {VENV_PY} {' '.join(sys.argv)}")
            sys.exit(2)
        print("❌ This step needs torch and transformers, which are deliberately")
        print("   not dependencies of the quality gate. Install them yourself.")
        print()
        print("   Both pinned models are sized to run on CPU; a GPU is used")
        print("   automatically when present. For a CPU-only box, add")
        print("   --index-url https://download.pytorch.org/whl/cpu to the torch line.")
        print("     python3 -m venv .venv")
        print("     .venv/bin/pip install torch")
        print("     .venv/bin/pip install transformers torchao accelerate")
        print("   then run this with .venv/bin/python")
        print()
        print(f"   Models (pinned): {MODELS['embedder']}")
        print(f"                    {MODELS['verdict']}  ({QUANT}, CPU)")
        print("   `list` and `chunks` work without any of this.")
        print()
        print(f"   Models cache to {MODEL_CACHE} (set by this script), not ~/.cache.")
        sys.exit(2)


@dataclass
class Candidate:
    path: str
    fn: str
    line: int
    code: str
    score: float = 0.0

    def key(self) -> tuple:
        """Identity for dedup: several windows of one function are one finding."""
        return (self.path, self.fn)


def windows(code: str, first_line: int):
    """Overlapping slices of a function body, as (line, text).

    A body shorter than one window is returned whole -- slicing it further would
    just produce fragments with less context than the pattern they are compared
    against.
    """
    rows = code.splitlines()
    if len(rows) <= WINDOW_LINES:
        yield first_line, code
        return
    for start in range(0, len(rows) - WINDOW_LINES + 1, WINDOW_STRIDE):
        yield first_line + start, "\n".join(rows[start : start + WINDOW_LINES])


def collect_functions(rev: str | None = None) -> list[Candidate]:
    """Every production function in the tree, as an embeddable chunk."""
    out: list[Candidate] = []
    if rev is None:
        files = [(str(p), p.read_text(errors="ignore")) for p in rust_files(Path("."))]
    else:
        names = subprocess.run(
            ["git", "ls-tree", "-r", "--name-only", rev],
            capture_output=True, text=True, check=True).stdout.split()
        files = []
        for f in names:
            if not f.endswith(".rs") or f.startswith("scratch/"):
                continue
            files.append((f, subprocess.run(["git", "show", f"{rev}:{f}"],
                                            capture_output=True, text=True).stdout))
    for path, src in files:
        prod = production_code(src)
        for name, line, body, _at in function_bodies(prod, r"\w+"):
            for wline, wtext in windows(body, line):
                out.append(Candidate(path, name, wline, wtext))
    return out


def embed(texts: list[str]):
    """Mean-pooled, L2-normalised GraphCodeBERT embeddings.

    Mean pooling, not CLS: GraphCodeBERT has no sentence-level training
    objective, so its CLS token carries no reliable whole-snippet meaning. The
    attention mask is applied so padding does not drag the vector toward zero.
    """
    _require_ml()
    import torch
    from transformers import AutoModel, AutoTokenizer

    MODEL_CACHE.mkdir(parents=True, exist_ok=True)
    dev = pick_device()
    tok = AutoTokenizer.from_pretrained(MODELS["embedder"])
    model = AutoModel.from_pretrained(MODELS["embedder"]).eval().to(dev)

    vecs = []
    with torch.no_grad():
        for i in range(0, len(texts), 32):
            batch = tok(texts[i : i + 32], padding=True, truncation=True,
                        max_length=512, return_tensors="pt").to(dev)
            hidden = model(**batch).last_hidden_state
            mask = batch["attention_mask"].unsqueeze(-1).float()
            pooled = (hidden * mask).sum(1) / mask.sum(1).clamp(min=1e-9)
            vecs.append(torch.nn.functional.normalize(pooled, dim=-1).cpu())
    return torch.cat(vecs)


REMOVE_PCS = 1

# Functions are compared in windows, not whole.
#
# An embedding summarises everything it is given, so a 174-line decode function
# embeds as "a large decode function" and the two lines that actually hold the
# defect contribute almost nothing. Measured: of the eight patterns, the only
# three whose real site was retrieved were the three whose target happened to be
# about the same size as the pattern (4-7 lines). All five failures had targets
# 15-30x larger than the query.
#
# Windowing makes both sides comparable -- a 6-line pattern competes against
# 12-line slices rather than against whole functions. The stride overlaps so a
# pattern straddling a boundary is still caught whole by the next window.
WINDOW_LINES = 12
WINDOW_STRIDE = 6


def decorrelate(corpus_vecs, query_vecs):
    """Strip the common direction that makes every cosine look high.

    GraphCodeBERT is pretrained with masked-LM and data-flow objectives, not a
    contrastive one, so its raw embeddings are anisotropic: they sit in a narrow
    cone around a dominant mean direction, and every pair of vectors inherits
    that direction's overlap. Measured on this tree, raw cosine between unrelated
    functions had p50 0.882. After subtracting the mean it is -0.026, and the
    p05-p95 span goes from 0.184 to 1.086 -- about six times more usable range,
    from one subtraction.

    This was not a limitation of the model. An earlier version of this file read
    the compressed distribution, concluded that Rust must be out of
    CodeSearchNet's distribution, and swapped the embedder out. The swap
    "improved" things only because modern embedding models ship contrastively
    tuned, so it compared a model used correctly against one used wrongly.

    Removing the top principal component as well trades a little more range for
    a little less signal (span 0.727); past ~3 components it starts discarding
    real structure, so `REMOVE_PCS` stays at 1.

    The statistics are fitted on the repository's own functions and applied to
    the query patterns too -- they have to share one space, or the scores mean
    nothing. That makes the fitted corpus part of the determinism contract,
    alongside the model revisions and the thresholds.
    """
    import torch

    mean = corpus_vecs.mean(0, keepdim=True)
    c = corpus_vecs - mean
    q = query_vecs - mean
    if REMOVE_PCS:
        _u, _s, v = torch.pca_lowrank(c, q=min(16, len(c) - 1))
        basis = v[:, :REMOVE_PCS]
        c = c - (c @ basis) @ basis.T
        q = q - (q @ basis) @ basis.T
    n = torch.nn.functional.normalize
    return n(c, dim=-1), n(q, dim=-1)


def retrieve(rev: str | None = None):
    """Top-K nearest neighbours of each exemplar, above MIN_SIMILARITY."""
    import torch

    corpus = load_corpus()
    for ex in corpus:
        ok, detail = resolve(ex)
        if not ok:
            print(f"❌ exemplar {ex.id}: {detail}")
            return None, None
    cands = collect_functions(rev)
    print(f"  embedding {len(cands)} functions + {len(corpus)} exemplars ...")
    # The *pattern* is the query, not the function it came from -- see the
    # corpus comment on why embedding the whole function retrieves the wrong
    # thing.
    all_vecs = embed([c.code for c in cands] + [e.pattern for e in corpus])
    cv, ev = decorrelate(all_vecs[: len(cands)], all_vecs[len(cands) :])

    hits: dict[str, list[Candidate]] = {}
    for i, ex in enumerate(corpus):
        sims = (cv @ ev[i]).tolist()
        ranked = sorted(zip(sims, cands), key=lambda t: -t[0])
        picked, seen = [], set()
        for score, c in ranked:
            if len(picked) >= ex.top_k or score < ex.min_sim:
                break
            # The exemplar's own function is a guaranteed match and says nothing.
            if c.path == ex.file and c.fn == ex.fn:
                continue
            # Several windows of one function are one candidate, not several.
            if c.key() in seen:
                continue
            seen.add(c.key())
            picked.append(Candidate(c.path, c.fn, c.line, c.code, score))
        hits[ex.id] = picked
    return corpus, hits


# One fact, one call, one token.
#
# Two earlier versions failed in opposite directions, and both failures were the
# prompt rather than the model. Asking "does this contain the same defect?" got
# YES on 16 of 62 -- the question invites agreement. Replacing it with three
# chained checks plus 30 words of reasoning got YES on all 27: more room to fill,
# more agreement. A 0.8B model has short attention, so the fix is less prompt,
# not more structure.
#
# What is left is a question the model can answer by looking: "does this code
# call env::var?" is recognition, not judgement. No defect description, no
# exemplar, no reasoning space -- those all pull toward the answer the prompt
# implies.
VERDICT_PROMPT = """```rust
{candidate}
```

{question}
Answer YES or NO."""


def verdict(tok, model, ex: Exemplar, cand: Candidate) -> bool:
    """True only when every check answers the way the pattern expects.

    Separate calls, not one prompt with several questions: a short-attention
    model asked three things at once answers the last one and agrees with the
    rest. A cheap negative ("is there a cached static?" -> YES) disqualifies the
    candidate immediately, so most never reach the questions that need care.
    """
    import torch

    for chk in ex.checks:
        prompt = VERDICT_PROMPT.format(candidate=cand.code[:2500], question=chk["q"])
        msgs = [{"role": "user", "content": prompt}]
        try:
            text = tok.apply_chat_template(msgs, tokenize=False,
                                           add_generation_prompt=True,
                                           enable_thinking=False)
        except TypeError:
            text = tok.apply_chat_template(msgs, tokenize=False,
                                           add_generation_prompt=True)
        inputs = tok(text, return_tensors="pt", truncation=True,
                     max_length=4096).to(model.device)
        with torch.no_grad():
            out = model.generate(**inputs, max_new_tokens=3, do_sample=False,
                                 disable_compile=True,
                                 pad_token_id=tok.eos_token_id)
        reply = tok.decode(out[0][inputs["input_ids"].shape[1]:],
                           skip_special_tokens=True).strip().upper()
        answer = "YES" if reply.startswith("Y") else "NO"
        if answer != chk["want"]:
            return False
    return True


def cmd_scan() -> int:
    _require_ml()
    corpus, hits = retrieve()
    if corpus is None:
        return 1
    total = sum(len(v) for v in hits.values())
    print(f"  {total} candidate(s) after per-exemplar top_k / min_sim\n")
    if not total:
        print("✅ Nothing resembles a known anti-pattern")
        return 0
    tok, model = load_verdict_model()
    flagged = 0
    for ex in corpus:
        for c in hits[ex.id]:
            if verdict(tok, model, ex, c):
                flagged += 1
                print(f"  ⚠️  {c.path}:{c.line}  fn `{c.fn}`  (sim {c.score:.2f})")
                print(f"       resembles: {ex.id} — spec §{ex.spec}")
    print()
    print(f"{flagged} of {total} candidate(s) judged to be the defect.")
    print("This is a review queue, not a verdict. Read the code before acting.")
    return 0


def cmd_gold() -> int:
    """Measure the pipeline against commits with known answers.

    Retrieval only -- whether the exemplar's own defect is found at the revision
    where it existed. Without this number the tool's output is a suggestion of
    unknown quality, which is exactly the state `mimas-performance-analysis.md`
    was in this morning when it answered "Will Mimas Be More Performant? Yes."
    with no data.
    """
    _require_ml()
    corpus = load_corpus()
    # The corpus spans 8 exemplars but only a handful of revisions; embedding the
    # whole tree once per exemplar would redo the same work several times over.
    by_rev: dict[str, tuple] = {}
    ok = fail = 0
    for ex in corpus:
        got, detail = resolve(ex)
        if not got:
            print(f"  ❌ {ex.id}: {detail}")
            fail += 1
            continue
        if ex.rev not in by_rev:
            cands = collect_functions(ex.rev)
            print(f"  embedding {len(cands)} windows at {ex.rev} ...")
            by_rev[ex.rev] = (cands, embed([c.code for c in cands]))
        cands, cand_raw = by_rev[ex.rev]
        # Query with the minimal pattern; check whether it pulls up the real
        # site recorded in `rev`/`file`/`fn`. That is the question that matters:
        # does a hand-written example find the defect where it actually shipped?
        cand_vecs, ex_vec = decorrelate(cand_raw, embed([ex.pattern]))
        sims = (cand_vecs @ ex_vec[0]).tolist()
        ranked = sorted(zip(sims, cands), key=lambda t: -t[0])[: ex.top_k]
        found = any(c.path == ex.file and (ex.lines or c.fn == ex.fn)
                    for _s, c in ranked)
        print(f"  {'✅' if found else '❌'} {ex.id}: own site "
              f"{'in' if found else 'NOT in'} top-{ex.top_k} at {ex.rev} "
              f"(best {ranked[0][0]:.2f})")
        ok += found
        fail += not found
    print(f"\nrecall: {ok}/{ok + fail}")
    return 0 if fail == 0 else 1


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("command", choices=["list", "chunks", "scan", "gold"],
                    help="list: resolve the corpus (no ML). "
                         "chunks: count embeddable functions (no ML). "
                         "scan: find look-alikes. "
                         "gold: measure precision/recall against known answers.")
    args = ap.parse_args()
    return {"list": cmd_list, "chunks": cmd_chunks,
            "scan": cmd_scan, "gold": cmd_gold}[args.command]()


if __name__ == "__main__":
    sys.exit(main())
