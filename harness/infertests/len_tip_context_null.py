from pathlib import Path

def get_label_module(label):
    path = Path(label)
    if len(path.parts) == 1:
        return label.split(".")[0]
    return None

r = len(Path("x").parts)
r
