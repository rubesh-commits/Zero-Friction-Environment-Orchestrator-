import ast
import sys

def extract_third_party_imports(filepath):
    with open(filepath, "r") as file:
        try:
            tree = ast.parse(file.read())
        except SyntaxError as e:
            return {"error": f"Syntax error in code: {e}"}

    imports = set()
    
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                imports.add(alias.name.split('.')[0]) 
                
        elif isinstance(node, ast.ImportFrom):
            if node.module:
                imports.add(node.module.split('.')[0])

    std_libs = sys.stdlib_module_names
    third_party_imports = [mod for mod in imports if mod not in std_libs]

    return third_party_imports

if __name__ == "__main__":
    target_file = sys.argv[1] if len(sys.argv) > 1 else "heavy_model.py"
    found_imports = extract_third_party_imports(target_file)
    print(found_imports)