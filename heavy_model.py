import time

def print_ram_stats(phase_name):
    with open('/proc/meminfo', 'r') as f:
        lines = f.readlines()
    
    # Extract values in KB and convert to MB
    total_mb = int(lines[0].split()[1]) / 1024
    available_mb = int(lines[2].split()[1]) / 1024
    used_mb = total_mb - available_mb
    
    print(f"[{phase_name}]")
    print(f" -> RAM Used: {used_mb:.2f} MB")
    print(f" -> RAM Left: {available_mb:.2f} MB\n")

print("========== ML ENGINE STRESS TEST ==========\n")

print_ram_stats("1. INITIAL BOOT STATE")
time.sleep(2)

print("Allocating 4GB of tensor weights...\n")
# Simulating a massive 4GB model load into RAM
fake_tensors = bytearray(4 * 1024 * 1024 * 1024) 

print_ram_stats("2. PEAK TENSOR ALLOCATION")

print("Running mock inference for 5 seconds...")
time.sleep(5)

# Free the memory
del fake_tensors
print("\nInference complete. Tensors destroyed.")

print_ram_stats("3. POST-EXECUTION CLEANUP")
print("===========================================")
time.sleep(2)