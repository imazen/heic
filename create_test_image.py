#!/usr/bin/env python3
"""Generate a 120x120 test image with patterns that force non-zero residuals in all CTUs."""

from PIL import Image, ImageDraw
import random
import os

def create_test_image(filename="test_120x120.png", use_noise=True):
    """Create a 120x120 image with patterns in all CTU regions.
    
    CTU layout for 120x120 with 32x32 CTUs:
    - CTU 0: (0,0)   to (31,31)   - top-left
    - CTU 1: (32,0)  to (63,31)   - top-middle
    - CTU 2: (64,0)  to (95,31)   - top-right
    - CTU 3: (0,32)  to (31,63)   - middle-left
    - CTU 4: (32,32) to (63,63)   - center
    - CTU 5: (64,32) to (95,63)   - middle-right
    - CTU 6: (0,64)  to (31,95)   - bottom-left
    - CTU 7: (32,64) to (63,95)   - bottom-middle
    - CTU 8: (64,64) to (95,95)   - bottom-right
    """
    
    img = Image.new('RGB', (120, 120), color='black')
    draw = ImageDraw.Draw(img)
    
    # Define colors for each CTU region to ensure they all have different content
    ctu_colors = [
        (255, 0, 0),    # CTU 0: Red
        (0, 255, 0),    # CTU 1: Green
        (0, 0, 255),    # CTU 2: Blue
        (255, 255, 0),  # CTU 3: Yellow
        (255, 0, 255),  # CTU 4: Magenta
        (0, 255, 255),  # CTU 5: Cyan
        (128, 255, 128),# CTU 6: Light green
        (255, 128, 0),  # CTU 7: Orange
        (128, 0, 255),  # CTU 8: Purple
    ]
    
    # Fill each CTU region with a solid color + some pattern
    ctu_positions = [
        (0, 0), (32, 0), (64, 0),
        (0, 32), (32, 32), (64, 32),
        (0, 64), (32, 64), (64, 64),
    ]
    
    for i, ((x0, y0), color) in enumerate(zip(ctu_positions, ctu_colors)):
        # Fill 32x32 region with color
        for y in range(y0, min(y0 + 32, 120)):
            for x in range(x0, min(x0 + 32, 120)):
                if use_noise:
                    # Add slight noise to force non-zero residuals
                    noise_r = max(0, min(255, color[0] + random.randint(-20, 20)))
                    noise_g = max(0, min(255, color[1] + random.randint(-20, 20)))
                    noise_b = max(0, min(255, color[2] + random.randint(-20, 20)))
                    img.putpixel((x, y), (noise_r, noise_g, noise_b))
                else:
                    img.putpixel((x, y), color)
        
        # Add a distinct pattern in each CTU
        pattern_color = tuple(255 - c for c in color)
        if i % 3 == 0:
            # Draw a circle
            draw.ellipse([x0 + 8, y0 + 8, x0 + 24, y0 + 24], fill=pattern_color)
        elif i % 3 == 1:
            # Draw a rectangle
            draw.rectangle([x0 + 8, y0 + 8, x0 + 24, y0 + 24], fill=pattern_color)
        else:
            # Draw a cross
            draw.line([x0 + 8, y0 + 16, x0 + 24, y0 + 16], fill=pattern_color, width=4)
            draw.line([x0 + 16, y0 + 8, x0 + 16, y0 + 24], fill=pattern_color, width=4)
    
    img.save(filename)
    print(f"Created {filename} (120x120 RGB with 9 distinct CTU patterns)")
    
    return img

def convert_to_heic(input_png="test_120x120.png", output_heic="test_120x120.heic", quality=90):
    """Convert PNG to HEIC format."""
    
    # Try using pillow-heif if available
    try:
        from pillow_heif import register_heif_opener
        register_heif_opener()
        
        img = Image.open(input_png)
        img.save(output_heic, quality=quality)
        print(f"Converted to {output_heic} using pillow-heif (quality={quality})")
        return True
    except ImportError:
        print("pillow-heif not available")
    
    print("\nFailed to convert to HEIC. Please install pillow-heif: pip install pillow-heif")
    return False

if __name__ == "__main__":
    # Create test image with noise to force non-zero residuals
    img = create_test_image("test_120x120.png", use_noise=True)
    
    # Convert to HEIC
    success = convert_to_heic("test_120x120.png", "test_120x120.heic", quality=95)
    
    if success:
        print(f"\nTest files created:")
        print(f"  - test_120x120.png (reference)")
        print(f"  - test_120x120.heic (test input with full CTU coverage)")
        
        for f in ["test_120x120.png", "test_120x120.heic"]:
            if os.path.exists(f):
                size = os.path.getsize(f)
                print(f"  - {f}: {size} bytes")
    else:
        print("\nPNG file was created - you can manually convert it to HEIC")
