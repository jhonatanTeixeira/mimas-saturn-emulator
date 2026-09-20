#!/usr/bin/env python3
"""As regras do projeto que dá para verificar sem ler o código com olhos.

Só existem regras aqui que já falharam de verdade em algum projeto desta
família. Cada uma diz qual diretiva quebrou e onde. Sem allowlist em arquivo
separado: uma exceção se justifica no ponto de uso, com o comentário que a
mensagem indica, ou não é exceção.

Uso:
    python3 tools/project_rules.py            # verifica o repositório
    python3 tools/project_rules.py --self-test  # verifica o verificador
"""
import re
import sys
from pathlib import Path

# Vocabulário de implementação do Yabause/YabaSanshiro. A diretiva 4 do projeto
# proíbe consultar esses emuladores; se um nome desses aparece no nosso código,
# alguém abriu a fonte deles.
YABAUSE_NAMES = [
    "T1ReadByte", "T1ReadWord", "T1ReadLong", "T1WriteByte", "T1WriteWord",
    "T1WriteLong", "T2ReadByte", "T2ReadWord", "T2ReadLong",
    "MappedMemoryReadByte", "MappedMemoryReadWord", "MappedMemoryReadLong",
    "MappedMemoryWriteByte", "MappedMemoryWriteWord", "MappedMemoryWriteLong",
    "SH2_struct", "CurrentSH2", "MSH2", "SSH2", "yabsys", "YabauseThread",
    "Vdp1External", "Vdp2External", "Cs2Area", "ScuRegs", "SmpcRegs",
    "scsp_main", "sh2int", "vidsoft", "vidogl",
]
YABAUSE_PATHS = ["yabause/src", "yabassanshiro", "../yabause"]
FORBIDDEN_DEPS = ["glium", "winit", "glutin"]


class Finding:
    def __init__(self, path, line, rule, message):
        self.path, self.line, self.rule, self.message = path, line, rule, message

    def __str__(self):
        return f"   {self.path}:{self.line}  [{self.rule}] {self.message}"


def rust_files(root: Path):
    for p in sorted(root.rglob("*.rs")):
        if any(part in ("target", "archived", "legacy_src") for part in p.parts):
            continue
        yield p


def rule_no_port(root: Path):
    """Diretiva 4: nada vem do Yabause, nem código, nem consulta."""
    out = []
    word = re.compile(r"\b(" + "|".join(map(re.escape, YABAUSE_NAMES)) + r")\b")
    for p in rust_files(root):
        for i, line in enumerate(p.read_text(errors="replace").splitlines(), 1):
            m = word.search(line)
            if m:
                out.append(Finding(p, i, "no-port", f"nome de implementação do Yabause: `{m.group(1)}`"))
            for path in YABAUSE_PATHS:
                if path in line:
                    out.append(Finding(p, i, "no-port", f"referência a um emulador vizinho: `{path}`"))
    return out


def rule_no_archived(root: Path):
    """`archived/` é o mimas antigo. Nada novo pode depender dele."""
    out = []
    targets = list(rust_files(root)) + [root / "Cargo.toml"]
    for p in targets:
        if not p.exists():
            continue
        for i, line in enumerate(p.read_text(errors="replace").splitlines(), 1):
            if "archived/" in line or "archived::" in line:
                out.append(Finding(p, i, "no-archived", "depende de `archived/`, que é o projeto arquivado"))
    return out


def rule_forbidden_deps(root: Path):
    """Diretiva 3: OpenGL puro e headless. Sem camada de janela."""
    out = []
    cargo = root / "Cargo.toml"
    if not cargo.exists():
        return out
    for i, line in enumerate(cargo.read_text().splitlines(), 1):
        stripped = line.split("#", 1)[0]
        for dep in FORBIDDEN_DEPS:
            if re.match(rf"\s*{dep}\s*=", stripped):
                out.append(Finding(cargo, i, "headless-gl", f"`{dep}` é camada de janela; o projeto é EGL surfaceless"))
    return out


RULES = [rule_no_port, rule_no_archived, rule_forbidden_deps]


def check(root: Path):
    return [f for rule in RULES for f in rule(root)]


def self_test() -> int:
    """Resposta conhecida: cada regra acende no caso errado e cala no certo."""
    import tempfile

    cases = [
        ("fn f() { let x = T1ReadLong(a, b); }", "src/a.rs", "no-port", True),
        ("// entendido de yabause/src/scu.c:120", "src/a.rs", "no-port", True),
        ("fn f() { let x = read_long(a, b); }", "src/a.rs", "no-port", False),
        ("let masks = MSH2_LIKE;", "src/a.rs", "no-port", False),  # não é palavra isolada
        ("use archived::sh2;", "src/a.rs", "no-archived", True),
        ("mod cpu;", "src/a.rs", "no-archived", False),
    ]
    failures = 0
    for body, rel, rule, should_fire in cases:
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "src").mkdir()
            (root / rel).write_text(body)
            fired = any(f.rule == rule for f in check(root))
            if fired != should_fire:
                failures += 1
                print(f"❌ self-test: {rule} {'deveria acender' if should_fire else 'não deveria acender'}: {body!r}")

    with tempfile.TemporaryDirectory() as d:
        root = Path(d)
        (root / "Cargo.toml").write_text('[dependencies]\nglium = "0.1"\ngl = "0.14"\n')
        fired = [f for f in check(root) if f.rule == "headless-gl"]
        if len(fired) != 1:
            failures += 1
            print(f"❌ self-test: headless-gl acendeu {len(fired)} vezes, esperado 1")

    if failures:
        return 1
    print(f"✅ self-test: {len(cases) + 1}/{len(cases) + 1} casos com resposta conhecida")
    return 0


def main() -> int:
    if "--self-test" in sys.argv:
        return self_test()
    findings = check(Path("."))
    if not findings:
        print("✅ Nenhuma regra do projeto quebrada")
        return 0
    print(f"❌ {len(findings)} violação(ões):")
    for f in findings:
        print(f)
    print()
    print("   no-port: o Yabause não pode ser consultado nem citado (diretiva 4).")
    print("   no-archived: `archived/` é o projeto antigo e está fora dos limites.")
    print("   headless-gl: renderização é EGL surfaceless + `gl` cru (diretiva 3).")
    return 1


if __name__ == "__main__":
    sys.exit(main())
