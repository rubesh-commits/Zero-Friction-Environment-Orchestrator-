# Fresh Python Service

[![Python](https://img.shields.io/badge/python-3.12-blue.svg)](https://www.python.org/)

A tiny HTTP client demo used by the WinOrchestrator acceptance suite. It is
deliberately small, but it uses syntax that only exists in modern Python so the
deterministic analyzer must raise the baseline requirement without any AI help.

## Quick start

```bash
python -m venv .venv
.venv\Scripts\activate
pip install -r requirements.txt
python app.py
```

The service prints a greeting and exits.
