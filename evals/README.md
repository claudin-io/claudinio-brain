# Evals

Tests prove correctness; evals measure quality. A brain can satisfy every
bitemporal invariant and still be useless if recall never surfaces the answer.

```
cargo run --example eval                     # measure, fail on regression
cargo run --example eval -- --update-baseline # accept the new numbers
```

## The three suites measure different things

**`retrieval.jsonl`** — can recall find the right fact when the question is not
phrased like the fact? Paraphrase, accent drift, capitals, multi-token entity
names, rare identifiers buried in common words, PT and EN.

**`temporal.jsonl`** — what plain RAG gets wrong, and the reason this project
exists. Current value after N changes, value at a past instant, full history,
backdated writes, corrections, retractions, relations that changed.

**`graph.jsonl`** — questions whose answer is **not** lexically similar to the
query and only appears by walking a relation. This suite is what justifies
having a graph at all rather than a plain vector store.

## Reading the ablation table

Every suite runs against each channel alone and fused, so the marginal
contribution of each channel is a number rather than an opinion. That is how the
decision to keep or cut a channel gets made — notably whether the semantic
channel in Passo 5 earns the ~8 MB of model weights it costs.

## Baseline as of Passo 4 (lexical only)

```
suite        channels         n     R@1     R@5     R@10     MRR   top1
graph        alias            8   0.000   0.000    0.000   0.000  0.000
graph        bm25             8   0.125   0.750    0.750   0.375  0.125
graph        bm25+alias       8   0.000   0.750    0.750   0.313  0.000
retrieval    alias           20   0.675   0.700    0.700   0.650  0.650
retrieval    bm25            20   0.975   1.000    1.000   0.950  0.950
retrieval    bm25+alias      20   0.975   1.000    1.000   0.950  0.950
temporal     alias           21   0.893   0.952    0.952   0.905  0.905
temporal     bm25            21   0.813   1.000    1.000   0.905  0.857
temporal     bm25+alias      21   0.909   1.000    1.000   0.952  0.952
```

Three things this immediately surfaced, none of which were obvious beforehand:

**Fusion earns its keep on the temporal suite.** BM25 alone gets 0.813 R@1 and
alias alone 0.893; fused they reach 0.909. Independent signals agreeing is
evidence, and RRF is how that evidence compounds.

**The alias channel adds nothing on the retrieval suite** (0.975 either way).
It is not free — it is a second query per recall — and it survives only because
of what it does for the temporal suite. Worth revisiting if that changes.

**Fusion currently *hurts* the graph suite**: BM25 alone scores 0.125 top-1,
fused it drops to 0.000. This is not a fusion bug. For "de que pais vem o
produto_a", the alias channel correctly pins `produto_a` and surfaces
`produto_a fornecido_por acme` — a fact genuinely about the named entity — while
the answer lives one hop further on, at `acme pais brasil`. With no traversal,
ranking the link fact first is the best available behaviour; it is simply not
the answer.

That number is the target for Passo 6. The measurement exists before the
feature on purpose, so graph expansion's value lands as a number rather than an
opinion.

## Growing the datasets

The suites hold ~20 cases each, not the 100+ the plan sketched. Fabricating 80
more by hand would add bulk without adding signal — cases invented to fill a
quota tend to test the implementation that already exists. The intended growth
path is real questions from agent transcripts, added when recall gets one wrong.

`holdout.jsonl` is reserved for a slice that only runs before a release, as a
guard against tuning the ranker against the visible cases.
