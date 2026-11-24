"""An undocumented repository: no manifest, no README, no setup instructions."""

def roll(values):
    if (total := sum(values)) > 10:
        return total
    return 0


if __name__ == "__main__":
    print(f"undocumented tool result: {roll([1, 2, 3, 4])}")
