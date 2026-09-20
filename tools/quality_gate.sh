#!/usr/bin/env bash
# Quality gate do mimasv2 — determinístico, sem modelo, sem rede.
#
# Roda tudo e reporta no fim; um passo vermelho não aborta os outros, porque
# saber que três coisas quebraram vale mais do que saber que a primeira quebrou.
#
# Apertar um limiar é livre. Afrouxar exige MIMAS_OVERRIDE_REASON, e o resumo
# diz que o resultado saiu sob limiar afrouxado — um verde assim não é o mesmo
# verde.
set -uo pipefail
cd "$(dirname "$0")/.."

PASS=0; FAIL=0; SKIP=0
RESULTS=()
pass() { PASS=$((PASS+1)); RESULTS+=("✅ $1"); echo "✅ $1"; }
fail() { FAIL=$((FAIL+1)); RESULTS+=("❌ $1"); echo "❌ $1"; }
skip() { SKIP=$((SKIP+1)); RESULTS+=("⏭  $1"); echo "⏭  $1"; }
step() { echo; echo "── $1"; }

# --- limiares ---------------------------------------------------------------
# Medidos neste repositório em 2026-09-20. Só descem (erro, avisos) ou sobem
# (cobertura, trace) — nunca o contrário sem justificativa.
MAX_WARNINGS="${MIMAS_MAX_WARNINGS:-28}"        # avisos do clippy
MAX_MEAN_ERR="${MIMAS_MAX_MEAN_ERR:-1.66}"      # erro médio contra as capturas
MIN_TRACE_PCT="${MIMAS_MIN_TRACE_PCT:-92.1}"    # % do trace de referência
MIN_DIFF_COV="${MIMAS_MIN_DIFF_COV:-90}"        # cobertura das linhas mudadas
MIN_AUDIO_CORR="${MIMAS_MIN_AUDIO_CORR:-0.707}" # correlação do envelope de áudio
COVERAGE_RANGE="${MIMAS_COVERAGE_COMMITS:-HEAD}"
FRAMES="${MIMAS_FRAMES:-620}"
AUDIO_FRAMES="${MIMAS_AUDIO_FRAMES:-750}"

loosened=0
check_loosening() { # nome, valor_atual, padrão, direção(min|max)
    local name="$1" cur="$2" def="$3" dir="$4"
    local worse
    if [ "$dir" = "min" ]; then
        worse=$(awk -v a="$cur" -v b="$def" 'BEGIN{print (a<b)?1:0}')
    else
        worse=$(awk -v a="$cur" -v b="$def" 'BEGIN{print (a>b)?1:0}')
    fi
    if [ "$worse" = "1" ]; then
        loosened=1
        echo "⚠️  $name afrouxado: $cur (padrão $def)"
        if [ -z "${MIMAS_OVERRIDE_REASON:-}" ]; then
            echo "    Defina MIMAS_OVERRIDE_REASON=\"<motivo>\" para afrouxar um limiar."
            exit 2
        fi
    fi
}
check_loosening "MIMAS_MAX_WARNINGS"  "$MAX_WARNINGS"  28    max
check_loosening "MIMAS_MAX_MEAN_ERR"  "$MAX_MEAN_ERR"  1.66  max
check_loosening "MIMAS_MIN_TRACE_PCT" "$MIN_TRACE_PCT" 92.1  min
check_loosening "MIMAS_MIN_DIFF_COV"  "$MIN_DIFF_COV"  90    min
check_loosening "MIMAS_MIN_AUDIO_CORR" "$MIN_AUDIO_CORR" 0.707 min
[ -n "${MIMAS_OVERRIDE_REASON:-}" ] && echo "⚠️  motivo declarado: $MIMAS_OVERRIDE_REASON"

# --- 1. formatação ----------------------------------------------------------
step "1/11 formatação"
if cargo fmt --check >/dev/null 2>&1; then
    pass "formatação"
else
    fail "formatação — rode: cargo fmt"
fi

# --- 2. clippy: catraca de avisos ------------------------------------------
step "2/11 clippy (teto de $MAX_WARNINGS avisos)"
CLIPPY_OUT=$(cargo clippy --all-targets 2>&1)
if echo "$CLIPPY_OUT" | grep -qE '^error'; then
    fail "clippy: erro de compilação"
    echo "$CLIPPY_OUT" | grep -E '^error' | head -5
else
    W=$(echo "$CLIPPY_OUT" | grep -E '^warning: ' | grep -vcE 'generated [0-9]+ warning')
    if [ "$W" -le "$MAX_WARNINGS" ]; then
        pass "clippy: $W avisos (teto $MAX_WARNINGS)"
        [ "$W" -lt "$MAX_WARNINGS" ] && echo "   baixe MIMAS_MAX_WARNINGS para $W e o ganho não se perde"
    else
        fail "clippy: $W avisos, teto é $MAX_WARNINGS — os novos são seus"
        echo "$CLIPPY_OUT" | grep -E '^warning: ' | grep -vE 'generated [0-9]+ warning' | tail -5
    fi
fi
# `cargo clippy --fix` nunca: ele já transformou uma guarda de opcode em no-op
# neste projeto-irmão enquanto calava o aviso que apontava o problema.

# --- 3. testes --------------------------------------------------------------
step "3/11 testes"
if TEST_OUT=$(cargo test 2>&1); then
    # `cargo test` imprime uma linha de resultado por alvo (lib, bins, doc);
    # somar é a única contagem que não mente quando um alvo tem zero testes.
    pass "testes: $(echo "$TEST_OUT" | grep -oE '[0-9]+ passed' | grep -oE '[0-9]+' | awk '{s+=$1} END{print s+0}') passaram"
else
    fail "testes"
    echo "$TEST_OUT" | grep -E '^(test .* FAILED|failures:|error)' | head -10
fi

# --- 4. testes que não afirmam nada ----------------------------------------
step "4/11 testes que não afirmam nada"
if python3 tools/assertionless_tests.py; then
    pass "todo teste afirma algo"
else
    fail "teste sem asserção — escreva uma, ou marque // no-assert: <motivo>"
fi

# --- 5. regras do projeto ---------------------------------------------------
step "5/11 regras do projeto"
python3 tools/project_rules.py --self-test >/dev/null || { fail "project_rules.py: self-test quebrado (o verificador está errado, não o código)"; }
if python3 tools/project_rules.py; then
    pass "regras do projeto"
else
    fail "regra do projeto quebrada"
fi

# --- 6. cobertura das linhas mudadas ---------------------------------------
step "6/11 cobertura do diff ($COVERAGE_RANGE, piso ${MIN_DIFF_COV}%)"
if ! command -v cargo-tarpaulin >/dev/null; then
    skip "cobertura: cargo-tarpaulin não instalado (cargo install cargo-tarpaulin)"
elif [ "${MIMAS_SKIP_COVERAGE:-0}" = "1" ]; then
    skip "cobertura: desligada por MIMAS_SKIP_COVERAGE=1"
else
    # opt-level 0 na marra: o perfil de debug deste projeto compila otimizado, e com
    # otimização o tarpaulin perde a atribuição de linha — mediu 13 linhas de uma mudança
    # que tem 113, e as que faltavam apareciam como "cobertas" por não existirem no mapa.
    if CARGO_PROFILE_DEV_OPT_LEVEL=0 cargo tarpaulin --out Lcov --ignore-tests \
        --output-dir . --timeout 600 >/tmp/mimasv2_tarpaulin.log 2>&1; then
        if python3 tools/diff_coverage.py --lcov lcov.info --range "$COVERAGE_RANGE" --min "$MIN_DIFF_COV"; then
            pass "cobertura do diff ≥ ${MIN_DIFF_COV}% ($COVERAGE_RANGE)"
        else
            fail "cobertura do diff abaixo de ${MIN_DIFF_COV}% — escreva testes para as linhas listadas"
        fi
    else
        fail "cobertura: tarpaulin falhou (veja /tmp/mimasv2_tarpaulin.log)"
    fi
fi

# --- 7. vídeo contra as capturas reais -------------------------------------
step "7/11 vídeo: erro médio contra as capturas (teto $MAX_MEAN_ERR/255)"
if [ ! -f saturn_bios.bin ]; then
    fail "vídeo: saturn_bios.bin não está na raiz — sem ele não há medição"
elif [ ! -d stubs/captures ]; then
    fail "vídeo: stubs/captures não existe — sem referência não há medição"
else
    cargo build --release >/dev/null 2>&1
    rm -rf out && mkdir -p out
    if ./target/release/mimasv2 --frames "$FRAMES" --dump out >/tmp/mimasv2_run.log 2>&1; then
        CMP=$(./target/release/compare stubs/captures out --max-frame 728 2>&1 | tail -1)
        ERR=$(echo "$CMP" | grep -oE 'erro médio geral [0-9.]+' | grep -oE '[0-9.]+')
        if [ -z "$ERR" ]; then
            fail "vídeo: não consegui ler o erro médio da saída do compare"
            echo "   saída: $CMP"
        elif awk -v e="$ERR" -v m="$MAX_MEAN_ERR" 'BEGIN{exit !(e<=m)}'; then
            pass "vídeo: erro médio $ERR/255 (teto $MAX_MEAN_ERR)"
            awk -v e="$ERR" -v m="$MAX_MEAN_ERR" 'BEGIN{if(e<m) print "   baixe MIMAS_MAX_MEAN_ERR para " e}'
        else
            fail "vídeo: erro médio $ERR/255, teto é $MAX_MEAN_ERR — a imagem piorou"
        fi
    else
        fail "vídeo: a execução da BIOS falhou (veja /tmp/mimasv2_run.log)"
    fi
fi

# --- 8. execução contra o trace real ---------------------------------------
step "8/11 trace: % dos PCs da referência (piso ${MIN_TRACE_PCT}%)"
if [ ! -f bios_trace_no_game.txt ]; then
    fail "trace: bios_trace_no_game.txt não está na raiz"
else
    TR=$(./target/release/mimasv2 --frames 700 --trace-check bios_trace_no_game.txt 2>&1 | grep -E '^TRACE' | head -2)
    PCT=$(echo "$TR" | head -1 | grep -oE '\([0-9.]+%\)' | tr -d '()%')
    if [ -z "$PCT" ]; then
        fail "trace: não consegui ler a porcentagem da saída"
    elif ! awk -v p="$PCT" -v m="$MIN_TRACE_PCT" 'BEGIN{exit !(p>=m)}'; then
        fail "trace: $PCT% dos PCs da referência, piso é ${MIN_TRACE_PCT}% — a execução regrediu"
    elif echo "$TR" | grep -q 'opcodes divergentes'; then
        fail "trace: opcode divergente — executamos um opcode diferente do real no mesmo PC"
        echo "$TR" | grep 'opcodes divergentes'
    else
        pass "trace: $PCT% dos PCs da referência, 0 opcodes divergentes"
        awk -v p="$PCT" -v m="$MIN_TRACE_PCT" 'BEGIN{if(p>m) print "   suba MIMAS_MIN_TRACE_PCT para " p}'
    fi
fi

# --- 9. áudio: forma do envelope contra a captura real -----------------------
# Não é diagnóstico (docs/sound.md já avisa: um sample de diferença no ataque
# muda o arquivo inteiro sem dizer o que quebrou) — é piso, como o erro de
# vídeo. Compara loudness ao longo do tempo (RMS por janela de 50 ms),
# buscando o deslocamento de tempo que melhor alinha as duas curvas, porque a
# gravação de referência não começa no mesmo instante que a amostra 0 nossa.
step "9/11 áudio: correlação do envelope contra a captura real (piso $MIN_AUDIO_CORR)"
if [ ! -f saturn_bios.bin ]; then
    fail "áudio: saturn_bios.bin não está na raiz — sem ele não há medição"
elif [ ! -f stubs/captures/audio/boot.wav ]; then
    fail "áudio: stubs/captures/audio/boot.wav não existe — sem referência não há medição"
else
    if ./target/release/mimasv2 --frames "$AUDIO_FRAMES" --dump-audio /tmp/mimasv2_audio.wav >/tmp/mimasv2_audio_run.log 2>&1; then
        CMP=$(./target/release/compare_audio stubs/captures/audio/boot.wav /tmp/mimasv2_audio.wav 2>&1)
        CORR=$(echo "$CMP" | grep -oE 'best: [0-9.-]+' | grep -oE '[0-9.-]+$')
        if [ -z "$CORR" ]; then
            fail "áudio: não consegui ler a correlação da saída do compare_audio"
            echo "   saída: $CMP"
        elif awk -v c="$CORR" -v m="$MIN_AUDIO_CORR" 'BEGIN{exit !(c>=m)}'; then
            pass "áudio: correlação $CORR (piso $MIN_AUDIO_CORR)"
            awk -v c="$CORR" -v m="$MIN_AUDIO_CORR" 'BEGIN{if(c>m) print "   suba MIMAS_MIN_AUDIO_CORR para " c}'
        else
            fail "áudio: correlação $CORR, piso é $MIN_AUDIO_CORR — o som piorou"
        fi
    else
        fail "áudio: a execução da BIOS falhou (veja /tmp/mimasv2_audio_run.log)"
    fi
fi

# --- 10/11. vídeo do jogo atual ----------------------------------------------
# Trabalhamos com um jogo por vez: a referência sempre mora no mesmo lugar,
# não tem nome de jogo no caminho. Pula (não falha) enquanto não houver
# referência, e pula de novo enquanto o mimasv2 não souber rodar um jogo de
# verdade (o CD Block hoje é stub comportamental — ver AGENTS.md) — os dois
# lados têm de existir antes disso virar pass/fail.
step "10/11 vídeo do jogo atual"
if [ ! -d stubs/captures-game/frames ] || [ -z "$(ls -A stubs/captures-game/frames 2>/dev/null)" ]; then
    skip "vídeo do jogo: sem captura de referência em stubs/captures-game/frames/"
else
    skip "vídeo do jogo: referência existe, mimasv2 ainda não roda jogo — nada nosso para comparar"
fi

# --- 11/11. áudio do jogo atual -----------------------------------------------
step "11/11 áudio do jogo atual"
if [ ! -f stubs/captures-game/audio.wav ]; then
    skip "áudio do jogo: sem stubs/captures-game/audio.wav"
else
    skip "áudio do jogo: referência existe, mimasv2 ainda não roda jogo — nada nosso para comparar"
fi

# --- resumo -----------------------------------------------------------------
echo
echo "════════════════════════════════════════"
for r in "${RESULTS[@]}"; do echo "$r"; done
echo "────────────────────────────────────────"
echo "$PASS verde(s), $FAIL vermelho(s), $SKIP pulado(s)"
[ "$loosened" = "1" ] && echo "⚠️  limiar afrouxado: \"$MIMAS_OVERRIDE_REASON\" — este verde não é o verde normal"
[ "$SKIP" -gt 0 ] && echo "⚠️  um passo pulado não é um passo verde; diga isso ao reportar"
echo "════════════════════════════════════════"
exit $(( FAIL > 0 ? 1 : 0 ))
