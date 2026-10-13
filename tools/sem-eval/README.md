# sem-eval

Measures semantic retrieval on your own files (SPEC ch.5 §5, §6.8): indexes a folder with the
chosen model and dimension, then runs query→file samples and prints Recall@1/@10 and MRR.

    sem-eval --model embeddinggemma-2-Q8_0.gguf --folder D:\Docs --samples samples.tsv [--dim 256]
    sem-eval --model ... --folder ... --generate 50      # known-item queries from file contents

`samples.tsv`: one `query<TAB>file` per line; `file` is a path relative to the folder or a file
name. Run it after every change to chunking, prompts or dimension.
