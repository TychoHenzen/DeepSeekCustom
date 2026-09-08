# Encoding Intelligence into Procedure: How to Make ~7B Local Models Do 90% of Your Coding Work

## TL;DR
- **Your core instinct is correct and backed by evidence: a rigid, harness-enforced procedure beats trusting a weak model to self-manage.** The winning pattern is a fixed pipeline (localize → edit → validate) where the 7B model does narrow, verifiable subtasks, deterministic tools (grammar, compiler, linter, tests) gate every output, and you escalate to a frontier model only when a verifier fails after a bounded retry budget.
- **Small models fail at code editing and cross-file reasoning, not at the narrow steps.** A base Qwen2.5-Coder-7B in an Agentless pipeline resolves only ~7.6% of SWE-bench Verified versus ~50.8% for Claude 3.5 Sonnet in the *same* pipeline — but the same 7B finds the right file ~60% of the time. So route localization, boilerplate, renaming, and test scaffolding to the 7B; route patch generation on hard bugs and architecture to the frontier model.
- **Make invalid output impossible, not requested.** Use grammar-constrained decoding (XGrammar/GBNF/Outlines) so JSON and tool calls are always parseable, keep per-step context short (7B models degrade badly with long histories), cap self-repair at 3–4 iterations, and use compiler/test feedback — not the model's self-critique — as the escalation trigger.

## Key Findings

### 1. Cascades and routing really do cut cost — but the coding-specific numbers are more sobering than the headlines
FrugalGPT (Chen, Zaharia, Zou; Stanford; arXiv:2305.05176, TMLR 2024) established the pattern: run the cheap model first, score its answer, escalate only if the score is low. The paper reports that "FrugalGPT can match the performance of the best individual LLM (e.g. GPT-4) with up to 98% cost reduction or improve the accuracy over GPT-4 by 4% with the same cost." RouteLLM (Ong et al., LMSYS; arXiv:2406.18665, ICLR 2025) trains a router on human-preference data and reports "cost reductions of over 85% on MT Bench, 45% on MMLU, and 35% on GSM8K ... while still achieving 95% of GPT-4's performance," routing only ~26% of calls to GPT-4, with a best-router cost-saving ratio of 3.66× on MT-Bench at 95% quality.

The important caveat: those numbers come from conversational/classification benchmarks (MT-Bench, MMLU, GSM8K), not repository-scale coding. On coding, a malformed tool call is not "slightly lower quality" — it is a broken step that fails downstream. Coding-specific routing results are thinner and newer, but exist: one cascade study on a real agentic coding workload (RLM-Cascade, arXiv:2606.22840) reports 47.4% API cost reduction and a 1.83× p50 speedup versus a direct frontier baseline while matching or exceeding its quality, using a cheap model to draft and the frontier model to verify.

### 2. Confidence signals work, but not the naive ones
"When Does Confidence-Based Cascade Deferral Suffice?" (Jitkrittum et al., NeurIPS 2023; arXiv:2307.02764) shows that deferring when the small model's softmax confidence is low "works remarkably well in practice" — but fails in three specific situations that describe your setup exactly: when the downstream model is a specialist (frontier model better on a non-uniform subset), under label noise, and under distribution shift. For a coding harness, this means: raw token-probability confidence from a 7B model is an unreliable escalation trigger by itself. Prefer verifier-based signals (does it compile, do tests pass) which are deterministic and cheap, and agreement-based signals (do N samples agree) which the literature shows are more robust than single-model confidence.

### 3. Constrained decoding: settles the "does it hurt accuracy?" debate
This is the most directly actionable area for your "cannot produce valid JSON" problem.

- **"Let Me Speak Freely?" (Tam, Wu, Tsai, Lin, Lee, Chen; Appier AI Research/NTU; EMNLP 2024 Industry; arXiv:2408.02442)** reported: "Surprisingly, we observe a significant decline in LLMs' reasoning abilities under format restrictions. Furthermore, we find that stricter format constraints generally lead to greater performance degradation in reasoning tasks."
- **The dottxt rebuttal "Say What You Mean"** re-ran the same tasks on Llama-3-8B and found the opposite once prompts were matched apples-to-apples: GSM8K 0.77→0.78, Last Letter 0.73→0.77, Shuffle Object 0.41→0.44 — structured generation *improved* accuracy across the board. The paper's reported degradation was an artifact of mismatched prompts and using a second, lenient LLM as the answer parser.
- The reconciliation from primary research: constraining the *final answer format* while letting the model reason first in free text is the safe design. Forcing the model to reason *inside* a rigid schema is what hurts.

Evidence that grammars especially help small models:
- **NVIDIA's Bash-generation study** ran 13 small models on 299 tasks with grammar-constrained decoding: mean pass rate rose from 62.5% to 75.2%, and Qwen3-0.6B jumped from 16.7% to 59.2%.
- **Grammar-Constrained Decoding for logical parsing (Raspanti et al., ACL 2025 Industry)** found grammar constraints "consistently improve both syntactic correctness and accuracy," "especially beneficial for resource-constrained applications using smaller models."
- **"Generating Structured Outputs from Language Models: Benchmark and Studies" (arXiv:2501.10868)** found across frameworks that constrained decoding "consistently improves the performance of downstream tasks up to 4%" and can "speed up the generation process by 50%."

Tooling: **XGrammar (Dong, Ruan, Cai, Lai, Xu, Zhao, Chen; CMU/MLC-AI; arXiv:2411.15100)** reduces mask computation to "under 40 microseconds for JSON schemas, roughly 100x faster than earlier libraries," and is now the default backend in both vLLM and SGLang. llama.cpp GBNF grammars and Outlines are the other mature options.

The alternative — and often the most robust choice — is to have the 7B model emit loose free text and let deterministic code do the structuring. This moves structural work out of the generation loop entirely and sidesteps the "count the brackets" failure mode completely.

### 4. Decomposition: the granularity that works for 7B models
Least-to-most prompting (Zhou et al., arXiv:2205.10625) and Plan-and-Solve (Wang et al., 2023) established that breaking a problem into ordered subproblems raises accuracy. But for weak models specifically, the evidence favors *harness-enforced staged workflows* over asking the model to decompose itself.

**Agentless (Xia, Deng, Dunn, Zhang; UIUC; arXiv:2407.01489, FSE/ICSE 2025)** is the key reference: a fixed three-phase pipeline — localize the bug, generate the repair, validate with tests — that achieved "the highest performance (27.33%) and lowest cost ($0.34)" on SWE-bench Lite "compared with all existing open-source software agents" (the improved FSE version reaches 32.00%, 96/300), versus roughly $3.34 per issue for agent-based approaches. Its structure is exactly the "rigid procedure" you want. DeepSeek's Agentless Mini emphasizes "straightforward component decomposition, parallelization, and scalability."

The critical empirical finding for your architecture:
- A **base Qwen2.5-Coder-7B-Instruct in the Agentless pipeline resolves 7.6% of SWE-bench Verified** (SoRFT, Ma et al., arXiv:2502.20127), vs. **Claude 3.5 Sonnet at 50.8% in the same pipeline** — a ~43-point gap.
- But the same 7B model's localization is far better than its editing: **file-hit 59.8%, function-hit 51.2%, line-hit 17.2%**. The bottleneck is the edit step, confirmed by SWE-Tester (arXiv:2601.13713): "code editing step is the primary bottleneck ... small-sized open-source LLMs demonstrate much stronger performance for code localization ... as compared to code editing."
- A *tuned* 7B localizer reaches ~70.8% file-level Acc@1, only ~7 points behind Claude 3.5 Sonnet (77.7%) — localization is nearly frontier-competitive; editing is not.

So the reliable division of labor is: 7B handles localization, boilerplate, renaming, test scaffolding, and mechanical edits; frontier handles the actual patch logic on non-trivial bugs, cross-file reasoning, and architecture.

Standalone code-gen benchmarks show 7B models are genuinely strong at *self-contained* functions. Per the Qwen2.5-Coder Technical Report (Hui, Yang et al.; arXiv:2409.12186), Qwen2.5-Coder-7B base scores HumanEval pass@1 61.6% and MBPP 76.9%, and instruction tuning "improved the HumanEval pass@1 score from 61.6% to 88.4%" (independent harnesses report the instruct model in the ~77–88% range depending on setup). The gap between this HumanEval competence and the ~7.6% SWE-bench weakness is precisely the gap between "write an isolated function" and "reason across a repository."

### 5. Verification instead of trust: the highest-leverage principle
- **LEVER (Ni et al., ICML 2023; arXiv:2302.08468)**: train a verifier to judge programs by their execution results, then rerank samples. Consistently improved language-to-code generation across four datasets.
- **Best-of-N with a verifier** exploits the large pass@1 → pass@k gap: sample N candidates, keep the first that passes tests. This raises pass rates but "saturates quickly" for small models (arXiv:2606.16999), so N should be small (3–5).
- **Self-repair loops**: feed the compiler/test error back and ask for a fix. "Is Three the Magic Number?" (Kiecker et al., arXiv:2607.05197) studied repair budgets across code-gen, test-gen, and translation with low-cost LLMs and found "the first three to four repair iterations account for most achievable gains, while later iterations contribute only marginal improvements."
- **A crucial warning for weak models**: Olausson et al. (ICML 2023) found self-repair gains are "marginal and can only be seen with GPT-4" when you count the cost — weaker models often can't diagnose their own errors. And a placebo-controlled study on frozen small code models (arXiv:2606.31511) questions whether re-showing failing code even helps, versus just resampling. Implication: for a 7B, use the *external* verifier signal to trigger a retry or escalation; don't rely on the model's self-critique.

### 6. Context management: why short histories matter more for 7B models
"Lost in the Middle" (Liu et al., TACL 2024) is the anchor: accuracy follows a U-shape and drops by more than 30% when relevant info sits in the middle of a long context, across six model families. RoPE's long-term decay is the architectural cause. Smaller models degrade earlier and harder.

Practical pattern from production coding harnesses (Claude Code, Codex, JetBrains, Sourcegraph, Anthropic): keep a structured scratchpad — a JSON/typed object tracking goals, files, changes made, commands run and their results — and reconstruct a short, fresh prompt each step rather than appending an ever-growing transcript. Claude Code auto-compacts near ~95% of the window into a 9-section structured summary (task, repo state, changes, errors, pending work), strips the reasoning scratchpad before it enters context, and re-attaches recently-read files. JetBrains research comparing observation-masking vs. LLM summarization concludes a hybrid beats either alone. This is exactly the "full control over history" capability your harness already has — it is a major advantage, and for 7B models it is not optional.

## Details: A Recommended Architecture for Your Harness

Your harness has two properties most systems lack: per-message backend switching and full history rewriting. Build the whole design around those.

**Stage 0 — Spec (OpenSpec; frontier or 7B).** Use OpenSpec to turn the task into an explicit, checkable spec and a decomposed plan. Spec authoring is a reasoning-heavy step where a frontier model earns its cost on hard tasks; for routine changes a 7B can draft and a verifier (does the plan reference real files/symbols?) checks it. The spec becomes the single source of truth that feeds every later per-step prompt — this is your defense against lost-in-the-middle, because each step gets a short prompt containing only the relevant spec slice, not the whole conversation.

**Stage 1 — Localization (7B, grammar-constrained).** Ask the 7B "which files/functions must change?" and constrain the output to a grammar that only emits valid file paths and symbol names from the actual repo. This plays to the 7B's strength (localization ≈ 60% file-hit out of the box, ~70%+ if you fine-tune later) and the grammar makes the output always machine-parseable. Enforcement point: reject any path not in the repo index; retry with the rejection as feedback.

### Implemented Stage 1 contract

The repository index defaults to at most 10,000 text files and 64 MiB (`64 * 1024 * 1024` bytes) of indexed text. `procedure.repository_index.max_files` and `procedure.repository_index.max_total_bytes` in `settings.json` override these limits. A limit breach stops localization before model dispatch. The configured values pass unchanged into each run.

The lightweight Rust symbol scanner deliberately supports an ASCII subset. It indexes `struct`, `enum`, `trait`, `union`, `type`, `const`, `static`, `mod`, `fn`, and `macro_rules!` names. Function modifiers such as `async`, `const`, `unsafe`, `default`, and `extern` remain supported. Other Rust item forms and Unicode identifiers stay available as path-only targets. A localizer may omit `symbol`, but it may not invent a symbol that the index did not expose.

A structurally valid localization report stops at review. Its initial disposition is `Pending`. Approval changes it to `Approved`; rejection changes it to `Rejected`. Reports written before the review field load as `LegacyUnreviewed`, remain inspectable, and never count as approved. The first terminal decision wins. Repeating the same decision is idempotent, while attempting the opposite decision returns a state error. Every decision is scoped to one run ID.

Only an `Approved` report satisfies the named downstream consumer guard. `Pending`, `Rejected`, `LegacyUnreviewed`, missing, and stale or mismatched reports must stop before later model, patch, or verification work.

Ollama localization always retains the structured JSON Schema and uses the model's default reasoning behavior. Every shared effort setting omits `thinking`, `thinking_mode`, `reasoning_effort`, and equivalent provider-native reasoning controls. DeepSeek and CLI backend behavior remains separate.

The archived [initial smoke observation](../openspec/changes/archive/2026-08-24-add-procedure-localization-runner/notes.md) is immutable historical evidence. It records an earlier Ollama request rejected because the model did not support the sent thinking option. The current [procedure localization verification](procedure-localization-verification.md) records the hardened request path and a later live run. That later run passed transport and schema decoding but failed repository structural validation, so semantic review was not reached.

**Stage 2 — Edit generation (routed).** This is the bottleneck step. Route by difficulty:
- Mechanical edits (renames, signature changes, boilerplate, adding imports, test scaffolding) → 7B, output constrained to a diff/patch grammar.
- Substantive patch logic, multi-file changes, subtle bugs → frontier model.
- Decide the route from cheap signals: number of files touched, whether the spec flags the change as cross-cutting, and whether Stage 1 localization was confident/consistent across samples (agreement-based signal, not raw softmax).

**Stage 3 — Verification gate (deterministic, always).** Every edit passes through: (a) does it parse / does the grammar accept it; (b) does it compile / type-check; (c) does the linter pass; (d) do existing + new tests pass. This is your LEVER-style gate. Nothing reaches the user or the next step without passing. The verifier result — not the model's opinion — is the source of truth.

**Stage 4 — Bounded repair, then escalate.** On verifier failure, feed the exact error back to the 7B and retry. Cap at 3 attempts (the "first three to four iterations" finding). If still failing, escalate the *same* task — with the accumulated error messages and the spec slice — to the frontier model. If the frontier model fails its own 1–2 retries, surface to the human. This bounded ladder is the concrete realization of "detect weak-model failure and escalate."

**Escalation triggers (make them explicit and deterministic):**
1. Grammar/parse rejection after 1 retry → stay on 7B (usually fixable) or swap to a stricter grammar.
2. Compile/type/test failure after 3 repair iterations → escalate to frontier.
3. Localization disagreement across N=3–5 samples (low agreement) → escalate localization to frontier.
4. Spec flags the change as architectural/cross-file → route to frontier from the start.
5. Never use raw 7B token-confidence as the sole trigger (Jitkrittum et al.).

**Context construction per step (use your rewrite capability):**
- Feed only: the relevant spec slice, the target file(s), the specific error (if repairing), and a compact scratchpad. Never the full transcript.
- Keep the most important instruction at the *start or end* of the prompt, never buried in the middle.
- Maintain the scratchpad as a typed object (goals, files, changes, last error), regenerated each step.

**Enforcement points summary:** grammar constraints at every generation step (Stages 1, 2); deterministic verifier gate (Stage 3); retry budget of 3 before escalation (Stage 4); context length cap and start/end placement of key info (all stages).

## Recommendations

**Stage 1 (build now):**
1. Put grammar-constrained decoding on every 7B output that must be machine-read. Use XGrammar (via vLLM/SGLang) or llama.cpp GBNF. This alone solves your "can't produce valid JSON / can't count brackets" problem — the harness makes malformed output impossible.
2. Adopt the Agentless localize→edit→validate skeleton as your fixed procedure. Wire OpenSpec in as Stage 0.
3. Make the verifier gate (compile + lint + tests) mandatory and the sole source of truth. Never let the model self-assess pass/fail.

**Stage 2 (after the skeleton works):**
4. Route the edit step by difficulty signals (files touched, spec flags, localization agreement). Start conservative: send anything non-trivial to the frontier model, then pull the threshold back toward the 7B as you measure.
5. Set the repair budget to 3 iterations, then escalate. Log every escalation with its trigger so you can tune.
6. Use free-text-then-parse where a grammar is awkward (e.g., natural-language rationale): let the 7B write loosely, then a deterministic parser or a tiny constrained second pass extracts the structured fields.

**Stage 3 (optimization):**
7. Add best-of-N (small N, 3–5) with the test-verifier as the selector for the edit step on the 7B, before escalating — a cheap way to close some of the pass@1→pass@k gap.
8. Consider fine-tuning the 7B on your own localization traces; a tuned 7B localizer approaches frontier file-level accuracy and is the cheapest high-leverage improvement.
9. Track the metrics that should move your thresholds (below).

**Benchmarks/thresholds that change the plan:**
- If the 7B's post-gate success on mechanical edits is below ~70%, tighten the router (send more to frontier) or add fine-tuning.
- If frontier escalations exceed ~10–15% of steps, your difficulty router is mis-calibrated or the 7B needs fine-tuning — investigate before spending more.
- If repair iterations 4+ are still resolving a meaningful fraction, your feedback quality is poor (fix the error formatting) rather than raising the budget.
- If constrained decoding ever lowers task success on a step, move the reasoning outside the schema (reason free, then constrain only the final answer).

## Caveats
- **Coding-specific cascade savings are unproven at the 98% headline level.** The 98% (FrugalGPT) and 85% (RouteLLM) figures are from non-coding benchmarks. Coding routing results are more modest (e.g., ~47% on one agentic coding workload) and newer (mostly 2025–2026 preprints). Treat cost-saving projections as directional, and measure your own.
- **Several supporting sources are recent preprints, not peer-reviewed.** The most load-bearing SWE-bench numbers (SoRFT, SWE-Tester, and various 2025–2026 arXiv papers) should be treated as preprints. The established anchors are the original SWE-bench (ICLR 2024), Agentless (FSE 2025), LEVER (ICML 2023), Lost in the Middle (TACL 2024), and the confidence-deferral paper (NeurIPS 2023).
- **SWE-bench localization numbers may be inflated by contamination.** File-localization accuracy on SWE-bench Verified from issue text alone can be much higher than on fresh benchmarks, suggesting memorization. Your 7B's real-world localization on your private code may be lower than the ~60% benchmark figure — measure on your own repos.
- **Small models can *regress* when handed too much context.** Giving a 7B all candidate files (oracle retrieval) sometimes lowers success because it over-edits. Give it the *minimal* necessary context, not the maximal.
- **The self-repair evidence is mixed for weak models.** Gains are robust for strong models; for frozen 7Bs, resampling can be as good as feedback-driven repair. This is why the architecture leans on *external* verifier signals and escalation rather than expecting the 7B to debug itself.
- **The 90/10 split is a goal, not a guarantee.** On genuinely hard repository tasks the frontier share will be higher; on boilerplate-heavy work the 7B share can exceed 90%. The right split is workload-dependent — instrument it and let the measured escalation rate tell you where you actually are.
