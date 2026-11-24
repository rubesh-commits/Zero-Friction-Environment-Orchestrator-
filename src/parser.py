import ast
import sys
import os

def extract_third_party_imports(target_path):
    imports = set()
    std_libs = sys.stdlib_module_names
    local_modules = set()

    def parse_file(file_path):
        try:
            with open(file_path, "r", encoding="utf-8") as file:
                tree = ast.parse(file.read())
            for node in ast.walk(tree):
                if isinstance(node, ast.Import):
                    for alias in node.names:
                        imports.add(alias.name.split('.')[0]) 
                elif isinstance(node, ast.ImportFrom):
                    if node.module:
                        imports.add(node.module.split('.')[0])
        except Exception:
            pass 

    if os.path.isdir(target_path):
        for root, dirs, files in os.walk(target_path):
            # Tell os.walk to completely ignore hidden folders and venvs
            dirs[:] = [d for d in dirs if not d.startswith('.') and d not in ('venv', 'env', '__pycache__')]
            
            for d in dirs:
                local_modules.add(d)
            for file in files:
                if file.endswith(".py"):
                    local_modules.add(file[:-3])
                    parse_file(os.path.join(root, file))
    elif os.path.isfile(target_path):
        local_modules.add(os.path.basename(target_path)[:-3])
        parse_file(target_path)

    return [mod for mod in imports if mod not in std_libs and mod not in local_modules]

if __name__ == "__main__":
    target = sys.argv[1] if len(sys.argv) > 1 else "."
    found_imports = extract_third_party_imports(target)
    print(found_imports)