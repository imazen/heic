Summary: Critical Bug Found in Test Setup

The differential tracing revealed that the Rust decoder skips the first 2 CTUs (x=0-63), starting at CTU 2 (x=64). This happens because the extracted HEVC bitstream's slice has slice_segment_address = 2, not 0.

Evidence:

libde265: 417 split decisions, starts at (0,0)
Rust: 101 split decisions, starts at (64,0)
Root Cause: The HEVC extraction from the HEIC file is incomplete - it's extracting only a partial slice starting at CTU 2, while libde265 processes the full image including all CTUs from 0.

Next Step: The extraction tool needs to be fixed to output all slices starting from CTU 0, or the test file needs to be regenerated with a single slice covering the entire image.

The tracing infrastructure is now fully working - both decoders output compatible traces. Once the test file issue is resolved, we can re-run the comparison to find actual coefficient decoding bugs.