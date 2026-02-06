#!/usr/bin/env python3
"""Extract raw HEVC bitstream from HEIC file."""

import sys
import os
from PIL import Image
try:
    from pillow_heif import register_heif_opener
    register_heif_opener()
    PILLOW_HEIF_AVAILABLE = True
except ImportError:
    PILLOW_HEIF_AVAILABLE = False
    print("Warning: pillow-heif not available, trying basic extraction")

def extract_heic_bitstream(heic_path, output_path):
    """Extract raw HEVC bitstream from HEIC file."""
    
    if not PILLOW_HEIF_AVAILABLE:
        print("Error: pillow-heif is required for HEIC extraction")
        return False
    
    try:
        # Open HEIC file
        import pillow_heif
        heif_file = pillow_heif.open_heif(heic_path)
        
        # Get HEVC data from the first image
        if len(heif_file) == 0:
            print("No images found in HEIC file")
            return False
        
        # Get the bitstream data
        image_data = heif_file[0].data
        bitstream = heif_file[0].bitstream
        
        if not bitstream:
            print("No HEVC bitstream found in HEIC file")
            return False
        
        # Write raw HEVC bitstream
        with open(output_path, 'wb') as f:
            f.write(bitstream)
        
        print(f"Extracted HEVC bitstream to {output_path}")
        print(f"Size: {os.path.getsize(output_path)} bytes")
        return True
        
    except Exception as e:
        print(f"Error extracting HEVC: {e}")
        return False

if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("Usage: python extract_hevc.py input.heic output.hevc")
        sys.exit(1)
    
    input_file = sys.argv[1]
    output_file = sys.argv[2]
    
    if not os.path.exists(input_file):
        print(f"Input file not found: {input_file}")
        sys.exit(1)
    
    success = extract_heic_bitstream(input_file, output_file)
    sys.exit(0 if success else 1)
