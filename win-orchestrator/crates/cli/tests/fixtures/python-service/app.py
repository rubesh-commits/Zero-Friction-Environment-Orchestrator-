"""Acceptance fixture: predictable output, modern Python syntax."""

from __future__ import annotations


def classify(value: str) -> str:
    match value:
        case "ok":
            return "ready"
        case _:
            numbers = [1, 2, 3]
            if (count := len(numbers)) > 1:
                return f"values={count}"
            return "empty"


def main() -> None:
    name = "acceptance"
    print(f"python-service says hello to {name}: {classify('ok')}")


if __name__ == "__main__":
    main()
