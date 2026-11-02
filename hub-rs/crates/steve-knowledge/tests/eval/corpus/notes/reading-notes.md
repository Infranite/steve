# Reading notes: search and retrieval

## BM25

BM25 ranks documents by term frequency with saturation and document-length normalization. The two knobs are k1 (how fast term frequency saturates, usually 1.2) and b (length normalization, usually 0.75). SQLite FTS5 ships a built-in `bm25()` ranking function; column weights let a title match count more than a body match.

## Hybrid retrieval

Lexical search is precise on rare words, names and error codes; dense vector search handles paraphrases and synonyms. Reciprocal Rank Fusion (RRF) merges the two ranked lists without calibrating scores: each document gets the sum of 1/(k + rank), with k = 60 by convention.

## RAG pitfalls

- Chunks that are too small lose context; too large and the relevant sentence drowns. 600 to 1000 characters with some overlap is a reasonable default for prose.
- Always keep the source offsets so answers can cite the exact passage.
- Evaluate with a fixed question set and recall@k before tuning anything; otherwise improvements are anecdotes.

## Books

- Designing Data-Intensive Applications (Martin Kleppmann): replication, partitioning, transactions.
- Introduction to Information Retrieval (Manning et al.): inverted indexes, tf-idf, evaluation.
