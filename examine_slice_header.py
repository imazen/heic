data = open('reference.hevc', 'rb').read()
i = 0
pps_count = 0
while i < len(data) - 3:
    if data[i] == 0 and data[i+1] == 0 and (data[i+2] == 1 or (i+3 < len(data) and data[i+2] == 0 and data[i+3] == 1)):
        start = i + 3 if data[i+2] == 1 else i + 4
        if start < len(data):
            nal_type = (data[start] >> 1) & 0x3F
            if nal_type == 34:  # PPS
                pps_count += 1
                print(f'PPS NAL {pps_count} at offset {i}')
                print(f'  Header: {data[start:start+3].hex()}')
                print(f'  First 10 payload bytes: {data[start+3:start+13].hex()}')
        i = start
    else:
        i += 1
print(f'Total PPS NALs found: {pps_count}')