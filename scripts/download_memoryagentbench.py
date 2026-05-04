#!/usr/bin/env python3
"""
Download MemoryAgentBench from HuggingFace and convert to JSONL.

Usage:
  uv run --with huggingface_hub --with pyarrow scripts/download_memoryagentbench.py
"""
import json
from pathlib import Path

import pyarrow.parquet as pq
from huggingface_hub import hf_hub_download


DEST = Path("data/memoryagentbench")
SPLITS = [
    "Accurate_Retrieval",
    "Test_Time_Learning",
    "Long_Range_Understanding",
    "Conflict_Resolution",
]


def main():
    DEST.mkdir(parents=True, exist_ok=True)
    for split in SPLITS:
        fname = f"data/{split}-00000-of-00001.parquet"
        path = hf_hub_download(
            repo_id="ai-hyz/MemoryAgentBench",
            filename=fname,
            repo_type="dataset",
            local_dir=DEST / "_hf",
        )
        out = DEST / f"{split}.jsonl"
        table = pq.read_table(path)
        with out.open("w") as f:
            for row in table.to_pylist():
                f.write(json.dumps(row) + "\n")
        print(f"{split}: {table.num_rows} rows -> {out}")


if __name__ == "__main__":
    main()
