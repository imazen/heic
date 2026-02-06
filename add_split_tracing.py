#!/usr/bin/env python3
"""Add split_transform_flag tracing to libde265 slice.cc"""

import sys

trace_code = '''
#ifdef TRACE_SPLIT_DECISIONS
{
    static FILE* split_trace = nullptr;
    if (!split_trace) {
        split_trace = fopen("libde265_split_trace.txt", "w");
    }
    if (split_trace) {
        fprintf(split_trace, "SPLIT x0=%d y0=%d log2=%d depth=%d split=%d\\n",
                x0, y0, log2TrafoSize, trafoDepth, split_transform_flag);
        fflush(split_trace);
    }
}
#endif
'''

print("Add this code to libde265/libde265/slice.cc in decode_transform_tree()")
print("Right after split_transform_flag is decoded:\n")
print(trace_code)
