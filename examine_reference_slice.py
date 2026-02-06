data = open('reference.hevc', 'rb').read()

# Find slice NAL (type 19-21)
i = 0
while i < len(data) - 3:
    if data[i] == 0 and data[i+1] == 0 and (data[i+2] == 1 or (i+3 < len(data) and data[i+2] == 0 and data[i+3] == 1)):
        start = i + 3 if data[i+2] == 1 else i + 4
        if start < len(data):
            nal_type = (data[start] >> 1) & 0x3F
            if 19 <= nal_type <= 21:  # Slice NALs
                print(f'Slice NAL at offset {i}')
                print(f'NAL header: {data[start:start+3].hex()}')
                print(f'First 20 payload bytes: {data[start+3:start+23].hex()}')
                
                # Manual bit parsing for PPS ID
                payload = data[start+3:]  # Skip NAL header (3 bytes)
                print(f'Payload length: {len(payload)} bytes')
                
                # Show first 32 bits of payload
                if len(payload) >= 4:
                    first_32_bits = payload[0] << 24 | payload[1] << 16 | payload[2] << 8 | payload[3]
                    print(f'First 32 bits: 0x{first_32_bits:08x} ({first_32_bits:032b})')
                
                # Parse UE for PPS ID manually
                bit_pos = 0
                
                # Skip first_slice_segment_in_pic_flag (1 bit)
                first_slice_flag = (payload[0] >> 7) & 1
                print(f'first_slice_segment_in_pic_flag: {first_slice_flag}')
                bit_pos += 1
                
                # Parse PPS ID as UE
                leading_zeros = 0
                while True:
                    if bit_pos >= len(payload) * 8:
                        print('Reached end of payload while parsing PPS ID')
                        break
                    byte_idx = bit_pos // 8
                    bit_idx = 7 - (bit_pos % 8)
                    bit = (payload[byte_idx] >> bit_idx) & 1
                    if bit == 0:
                        leading_zeros += 1
                        bit_pos += 1
                    else:
                        break
                
                print(f'Leading zeros in PPS ID: {leading_zeros}')
                
                # Read the remaining bits for PPS ID
                value = 0
                for _ in range(leading_zeros + 1):
                    if bit_pos >= len(payload) * 8:
                        print('Reached end of payload while reading PPS ID bits')
                        break
                    byte_idx = bit_pos // 8
                    bit_idx = 7 - (bit_pos % 8)
                    bit = (payload[byte_idx] >> bit_idx) & 1
                    value = (value << 1) | bit
                    bit_pos += 1
                
                pps_id = value
                print(f'Parsed PPS ID: {pps_id}')
                
                # Also show what our slice investigator would read
                print('\n--- Slice investigator simulation ---')
                print(f'Would read PPS ID: {pps_id}')
                print(f'Available PPS IDs: [0]')
                if pps_id != 0:
                    print('ERROR: PPS ID mismatch!')
                else:
                    print('PPS ID matches')
                break
        i = start
    else:
        i += 1

if i >= len(data) - 3:
    print('No slice NAL found')
