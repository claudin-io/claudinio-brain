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

## Baseline as of Passo 5 (lexical + semantic)

```
suite        channels         n     R@1     R@5     R@10     MRR   top1
graph        alias            8   0.000   0.000    0.000   0.000  0.000
graph        bm25             8   0.125   0.750    0.750   0.375  0.125
graph        semantic         8   0.000   0.875    0.875   0.237  0.000
graph        bm25+alias       8   0.000   0.750    0.750   0.313  0.000
graph        all              8   0.000   1.000    1.000   0.358  0.000
retrieval    alias           20   0.675   0.700    0.700   0.650  0.650
retrieval    bm25            20   0.975   1.000    1.000   0.950  0.950
retrieval    semantic        20   0.825   1.000    1.000   0.842  0.800
retrieval    bm25+alias      20   0.975   1.000    1.000   0.950  0.950
retrieval    all             20   0.975   1.000    1.000   0.950  0.950
temporal     alias           21   0.893   0.952    0.952   0.905  0.905
temporal     bm25            21   0.813   1.000    1.000   0.905  0.857
temporal     semantic        21   0.909   1.000    1.000   0.952  0.952
temporal     bm25+alias      21   0.909   1.000    1.000   0.952  0.952
temporal     all             21   0.909   1.000    1.000   0.952  0.952
```

### Does the semantic channel earn its 7.3 MB?

The plan's rule was to cut it if it added under 3 points of Recall@10 over
`bm25+alias`. Measured, against that row:

| suite | R@10 lexical | R@10 with semantic | delta |
|---|---|---|---|
| retrieval | 1.000 | 1.000 | **0** |
| temporal | 1.000 | 1.000 | **0** |
| graph | 0.750 | **1.000** | **+25 pts** |

**It stays, but only on the graph suite's evidence, and only for recall depth.**
Top-1 accuracy is unchanged everywhere: 0.950 on retrieval, 0.952 on temporal,
0.000 on graph with or without it. What the channel actually buys is getting the
one-hop answer *into the candidate set* — which is exactly the input graph
expansion needs in Passo 6. If Passo 6 fails to convert that recall into
precision, this channel should be re-examined rather than kept out of habit.

Worth noting on its own: `semantic` alone matches the fused lexical channels on
the temporal suite (0.909 R@1, 0.952 MRR). Static embeddings are doing more work
here than their ~82%-of-MiniLM MTEB reputation suggests, probably because facts
are short and structured rather than prose.

## Earlier baselines

### Passo 4 (lexical only)

```
graph        bm25             8   0.125   0.750    0.750   0.375  0.125
graph        bm25+alias       8   0.000   0.750    0.750   0.313  0.000
retrieval    bm25            20   0.975   1.000    1.000   0.950  0.950
temporal     bm25            21   0.813   1.000    1.000   0.905  0.857
temporal     bm25+alias      21   0.909   1.000    1.000   0.952  0.952
```

Findings that still hold: fusion earns its keep on the temporal suite (0.813 and
0.893 alone, 0.909 fused); the alias channel adds nothing on retrieval and
survives only on its temporal contribution; and fusion *hurts* graph top-1
(0.125 bm25-only to 0.000 fused) because the alias channel correctly pins the
named entity and surfaces the link fact while the answer sits one hop further
on. That 0.000 is still the number Passo 6 has to move.

## Growing the datasets

The suites hold ~20 cases each, not the 100+ the plan sketched. Fabricating 80
more by hand would add bulk without adding signal — cases invented to fill a
quota tend to test the implementation that already exists. The intended growth
path is real questions from agent transcripts, added when recall gets one wrong.

`holdout.jsonl` is reserved for a slice that only runs before a release, as a
guard against tuning the ranker against the visible cases.
