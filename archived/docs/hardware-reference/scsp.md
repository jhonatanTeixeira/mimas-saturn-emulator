# SCSP — Saturn Custom Sound Processor (32 voice slots, EG/LFO, sound DSP, DMA, M68K bus)

**Source of truth.** Everything in this document is derived *exclusively* from the Yabause
(YabaSanshiro fork) C source listed below. No external Saturn documentation, no SCSP/YMF292
data sheet, and no general audio-synthesis knowledge has been used to fill gaps.

| File | Lines | Role |
|---|---|---|
| `yabause/src/scsp.c` | 7122 | The SCSP implementation actually compiled by default. Contains **two** complete, mutually-exclusive synthesis engines (see §0). |
| `yabause/src/scsp.h` | 201 | Public interface only. Contains *no* register offset constants. |
| `yabause/src/scsp2.c` | 3560 | A **separate, compile-time-alternative** SCSP implementation (see §0.2). Not built by default. |
| `yabause/src/scsp2.h` | 162 | Header for the above; deliberately `#define`s `SCSP_H` so it *substitutes for* `scsp.h`. |
| `yabause/src/scspdsp.c` | 662 | The SCSP's onboard 128-step sound DSP (distinct from the SCU DSP). |
| `yabause/src/scspdsp.h` | 190 | `ScspDsp` state struct + the MPRO instruction-word bitfield union. |
| `yabause/src/sndmac.c` | 362 | **Not sound-RAM DMA.** This is the macOS CoreAudio *host output backend* (see §0.4). |
| `yabause/src/sndmac.h` | 27 | `#define SNDCORE_MAC 3`; declares `SNDMac`. |
| `yabause/src/m68kcore.c` | 179 | **Contains no Saturn memory map.** Core-selection dispatch + a dummy M68K (see §0.5). |
| `yabause/src/m68kcore.h` | 81 | `M68K_struct` vtable; core IDs. |

Four facts that these files cannot supply on their own are marked inline and cited to their own
file: the SH-2-side base addresses of sound RAM and the SCSP register block
(`yabause/src/memory.c:663-670`), the build-time selection between `scsp.c` and `scsp2.c`
(`yabause/src/CMakeLists.txt:200-207`), the definition of the `scsp_dsp` global
(`yabause/src/yabause.c:128`), and `SCSP_FRACTIONAL_BITS` (`yabause/src/yabause.h:154`).

**Tagging.** Where the code is ambiguous, self-contradictory, or plainly defective, that is
stated rather than smoothed over. Notes are tagged **[QUIRK]** (deliberate emulator shortcut or
unimplemented hardware behaviour) or **[BUG]** (a defect in the C source: wrong bit, swapped
field, dead code, missing `return`). Anything the source does not implement is called out as
*not implemented in this source* rather than guessed at.

**Citations** are of the form `scsp.c:1450`, `scspdsp.c:154`, etc. — they point at the code that
establishes the claim.

---

## 0. Structural overview — which file is which

### 0.1 `scsp.c` contains two engines, selected at *runtime*

`scsp.c` is not one SCSP model. It is two, sharing one register-decode front end, one global
control-register block, one timer, one interrupt controller and one M68K bus, but with
completely separate slot state and completely separate sample generation. The selector is the
global `int use_new_scsp` (`scsp.c:158`, default `0`), set via `scsp_set_use_new()`
(`scsp.c:1579-1585`), which is called from `yabause.c:449` out of the frontend's init struct.

| | "Old" engine | "New" engine |
|---|---|---|
| Selected when | `use_new_scsp == 0` (default) | `use_new_scsp == 1` |
| Attribution | Stephane Dallongeville 2004 / Theo Berkau 2004-2006 (`scsp.c:1-3`) | Theo Berkau 2015 (`scspdsp.c:1`), same era as the DSP |
| Slot state type | `slot_t` (`scsp.c:1637-1716`), global `scsp_t scsp` (`scsp.c:1801`) | `struct Slot` (`scsp.c:337-344`), global `struct Scsp new_scsp` (`scsp.c:346-352`) |
| Slot register writes | `scsp_slot_set_b/w` (`scsp.c:2147`, `2444`) | `scsp_slot_write_byte/word` (`scsp.c:1022`, `1257`) |
| Slot register reads | raw shadow `scsp_isr` (`scsp.c:2678-2700`) | reconstructed from fields (`scsp.c:1144`, `1346`) |
| Sample generation | `scsp_update()` — per-slot, block-at-a-time, 32 specialised C functions (`scsp.c:3843`) | `generate_sample()` — 32-step, 7-stage pipeline, one output sample at a time (`scsp.c:1450`) |
| Envelope model | fixed-point counter + rate tables + phase function pointers | attenuation register + step tables + modulus tables |
| Sound DSP (`scspdsp.c`) | **not used at all** | used (`scsp.c:1515-1516`) |
| Modulation (MDL/MDXSL/MDYSL) | parsed, never used | implemented (`scsp.c:540-555`) |
| Sound stack (`$600-$67E`) | `scsp.stack[64]` declared, never written | `new_scsp.sound_stack[64]`, written by `op7` (`scsp.c:824-832`) |
| MVOL (master volume) | **ignored** | applied (`scsp.c:1550-1555`) |
| CDDA mix point | mixed into the output buffer post-hoc (`scsp.c:3897-3936`) | fed to `EXTS0/1` and thence the DSP (`scsp.c:5321-5333`, `1501-1502`) |

Both engines share: the CCR block `$400-$42F` (`scsp_set_b/w`, `scsp_get_b/w`), timers
(`scsp_update_timer`), interrupts, MIDI, DMA (`scsp_dma`), the M68K bus handlers, and the
`scsp_reg[0x1000]` raw shadow.

Because the new engine is the only one with a DSP, modulation, a sound stack and master volume,
**it is the more complete hardware model and is the one Mimas should mirror**; the old engine is
documented here as well because it is the compiled default and because several bit decodings
only appear there.

### 0.2 `scsp2.c` is a third, compile-time-alternative implementation

`scsp2.h:21-22` states it outright:

```c
#ifndef SCSP_H  // Not SCSP2_H (we substitute for the original scsp.h)
#define SCSP_H
```

and `scsp.h:40-43` cooperates:

```c
// Quick hack so nobody else needs to know we're using a different header file
#ifdef USE_SCSP2
#include "scsp2.h"  // defines SCSP_H
#endif
```

The build makes them mutually exclusive (`yabause/src/CMakeLists.txt:200-207`):

```cmake
option(YAB_USE_SCSP2 "Use the new SCSP implementation.")
if (YAB_USE_SCSP2)
    add_definitions(-DUSE_SCSP2=1)
    set(yabause_SOURCES ${yabause_SOURCES} scsp2.c)
    ...
else()
    set(yabause_SOURCES ${yabause_SOURCES} scsp.c)
endif()
```

So: **`scsp.c` and `scsp2.c` are two versions of the same thing — old (default) vs. rewritten
(opt-in) — not two cooperating halves.** `scsp2.c` (Andrew Church, 2010, `scsp2.c:1-5`) is a
threadable rewrite of the *old* `scsp.c` engine: same envelope/LFO maths, cleaner register
decode, an M68K-driving thread with a shared write buffer, and **no sound DSP whatsoever**
(`scsp2.c:2754-2755`: *"we assume the data is being passed through the DSP (which we don't
currently implement)"*). `scspdsp.c` is unconditionally compiled (`CMakeLists.txt:54`) but
`scsp2.c` never references it.

`scsp2.c` is nevertheless the single best-commented description of the register layout in the
tree — its `SlotState_struct` (`scsp2.c:226-333`) and `ScspState_struct` (`scsp2.c:339-441`)
annotate every field with its exact bit range. Those comments are used as the *skeleton* of the
register tables below, and every field is cross-checked against the actual decode code in all
three engines.

### 0.3 Register-block layout constants

There is no header of offset `#define`s anywhere. Every offset is a literal in a `switch`. The
block is 12 bits wide (`a &= 0xFFF` in every entry point: `scsp.c:4135`, `4249`, `4333`, `4380`,
`4423`, `4515`; `scsp2.c:1579`, `1605`, `1623`). The raw shadow is carved up at
`scsp.c:4615-4617`:

| Symbol | Base | Covers |
|---|---|---|
| `scsp_isr` | `scsp_reg[0x0000]` | `$000-$3FF` — 32 slots × `$20` bytes (Individual Slot Registers) |
| `scsp_ccr` | `scsp_reg[0x0400]` | `$400-$42F` — Common Control Registers |
| `scsp_dcr` | `scsp_reg[0x0700]` | `$700-$FFF` — DSP register banks (mostly a dead shadow, see §7) |

All three arrays are byte-swapped: byte accesses index `[a ^ 3]`, word accesses `[a ^ 2]`
(`scsp.c:2155`, `2451`, `2713`, `2883`). `scsp2.c` instead keeps a straight `u16
scsp_regcache[0x1000/2]` (`scsp2.c:522`).

### 0.4 `sndmac.c` is **not** sound-RAM DMA

The task brief lists `sndmac.c` as "sound RAM DMA (main-RAM ↔ sound-RAM transfer)". It is not.
`sndmac.c` is the **Mac OS X CoreAudio host sound-output driver** — one of the interchangeable
`SoundInterface_struct` backends (`sndmac.c:60-77`, `sndmac.h:23-25`), built only on Apple
platforms (`CMakeLists.txt:468-469`). It implements `SNDMacInit`/`UpdateAudio`/`GetAudioSpace`/
`MuteAudio`/`SetVolume` against an `AudioUnit`, with a 65536-byte ring buffer
(`sndmac.c:33`, `79-84`) and a 44100 Hz / 16-bit / stereo stream format (`sndmac.c:146-155`).
It contains no Saturn hardware emulation at all. Its only SCSP-relevant detail is
`macConvert32uto16s` (`sndmac.c:273-292`), which applies the user volume percentage and
saturates the SCSP's `s32` sample pairs to `s16`.

**The actual sound-RAM DMA engine is `scsp_dma()` in `scsp.c:1955-1996`** (and `ScspDoDMA()` in
`scsp2.c:3152-3191`). It is documented in §8. Note also that this DMA is *SCSP-register ↔
sound-RAM*, **not** main-RAM ↔ sound-RAM; main-RAM ↔ sound-RAM transfers are an SCU DMA
function and appear nowhere in these files.

### 0.5 `m68kcore.c` contains no memory map

`m68kcore.c` is the M68K core-selection dispatcher: `M68KInit(coreid)` walks `M68KCoreList[]`
and installs the matching `M68K_struct` vtable (`m68kcore.c:34-51`), defaulting to `M68KDummy`.
`m68kcore.h:25-29` names the four cores: `M68KCORE_DUMMY 0`, `M68KCORE_C68K 1`, `M68KCORE_Q68
2`, `M68KCORE_MUSASHI 3`. The vtable (`m68kcore.h:34-70`) is where the memory map *hooks* live —
`SetReadB/SetReadW/SetWriteB/SetWriteW` (byte and word only; **no long accessors**, the 68000
being a 16-bit-bus part) and `SetFetch(low, high, ptr)`.

The Saturn-specific map is installed by the SCSP, not by `m68kcore.c`: `scsp.c:5074-5082` and
`scsp2.c:932-940`. See §10.

The one Saturn-specific thing in `m68kcore.c` is a hack:

```c
static s32 FASTCALL M68KDummyExec(UNUSED s32 cycle) {
    T2WriteWord(SoundRam, 0x700, 0);   // ... 0x710, 0x720, 0x730,
    T2WriteWord(SoundRam, 0x740, 0);   // ... 0x750, 0x760, 0x770,
    T2WriteWord(SoundRam, 0x790, 0);
    T2WriteWord(SoundRam, 0x792, 0);
    return 0;
}
```

(`m68kcore.c:63-76`) — **[QUIRK]** with no M68K core selected, the dummy core zeroes ten fixed
sound-RAM words every "execution" so that SH-2-side sound drivers polling those handshake
locations do not hang. Those addresses are hard-coded and unexplained.

---

## 1. Address map and access widths

### 1.1 Where the block lives

Within these files the SCSP is only ever addressed by its 12-bit register offset. The absolute
bases come from outside the source set (`yabause/src/memory.c:663-670`):

| SH-2 address range | Target | Handler |
|---|---|---|
| `0x05A00000-0x05AFFFFF` | Sound RAM | `SoundRamRead/Write{Byte,Word,Long}` (`scsp.c:4868`, `4887`, `4944`, `4967`, `4985`, `5039`) |
| `0x05B00000-0x05BFFFFF` | SCSP registers | `scsp_r_b/w/d`, `scsp_w_b/w/d` (`scsp.c:4132-4540`) |

From the M68K the SCSP register block appears at `0x100000` and up (§10).

`scsp.c:50-51` carries a note from the original author:

```
// note: model2 scsp is mapped to 0x100000~0x100ee4 of the space, but seems to
//       have additional hw ports ($40a~$410)
```

which is why `$40A-$410` are decoded nowhere and `scsp2.c:371` labels them
*"unused (possibly used in the model 2 SCSP?)"*.

### 1.2 Access-width coverage matrix (`scsp.c`)

| Offset range | Contents | byte R | byte W | word R | word W | long R | long W |
|---|---|---|---|---|---|---|---|
| `$000-$3FF` | Slot registers (ISR) | yes `:4382` | yes `:4137` | yes `:4425` | yes `:4251` | yes `:4517` | yes `:4335` |
| `$400-$43F` | Common control (CCR) | yes `:4391` | yes `:4151` | yes `:4434` | yes `:4265` | yes `:4526` | yes `:4354` |
| `$440-$5FF` | unmapped | 0 | ignored | 0 | ignored | 0 | ignored |
| `$600-$67E` | Sound stack (new engine) | 0 | ignored | yes `:4438` | ignored | 0 | ignored |
| `$680-$6FE` | Sound-stack mirror | 0 | ignored | yes `:4441` | ignored | 0 | ignored |
| `$700-$77F` | DSP `COEF[0..63]` | **0** | yes `:4162` | yes `:4444` | yes `:4272` | **0** | **ignored** |
| `$780-$7BF` | DSP `MADRS[0..31]` | **0** | yes `:4174` | partial `:4449` | yes `:4278` | **0** | **ignored** |
| `$800-$BFF` | DSP `MPRO[0..127]` | **0** | yes `:4185` | yes `:4460` | yes `:4283` | **0** | **ignored** |
| `$C00-$DFF` | DSP `TEMP` (nominal) | **0** | dead shadow `:4228` | **0** | dead shadow `:4312` | **0** | dead shadow |
| `$E00-$E7F` | DSP `MEMS` (nominal) | **0** | dead shadow | **0** | dead shadow | **0** | dead shadow |
| `$E80-$EBF` | DSP `MIXS[0..15]` | **0** | dead shadow | yes `:4485` | **dead shadow** | **0** | dead shadow |
| `$EC0-$EDF` | DSP `EFREG[0..15]` | yes `:4395` | dead shadow | yes `:4492` | **dead shadow** | **0** | yes `:4363` |
| `$EE0-$EE2` | DSP `EXTS0/1` | **0** | dead shadow | yes `:4494` | dead shadow | yes `:4534` | dead shadow |

"dead shadow" = the write lands in `scsp_dcr[]`/`scsp_reg[]` and has no effect on any emulated
state. Entries in bold are gaps; they are itemised in §14.

`scsp_r_w`/`scsp_w_w` log but do not reject misaligned addresses; they mask instead
(`a &= 0xFFE`, `scsp.c:4249`, `4423`). Long accessors mask `a &= 0xFFC` (`scsp.c:4333`, `4515`)
and decompose into two word accesses for `$000-$43F`.

---

## 2. Common Control Registers (CCR), `$400-$42F`

The canonical map is the author's own comment block at `scsp.c:57-83`, reproduced field-by-field
below and verified against `scsp_set_b` (`:2705`), `scsp_set_w` (`:2873`), `scsp_get_b`
(`:3000`), `scsp_get_w` (`:3054`) and `scsp2.c:2812-2941` / `2452-2463`.

### 2.1 Summary table

| Offset | Bits | Name | Meaning | Access | Decode |
|---|---|---|---|---|---|
| `$400` | 9 | `MEM4MB` | Sound RAM size: 0 = 2 Mbit (256 KB, mirrored), 1 = 4 Mbit (512 KB) | R/W | `scsp.c:2718`, `2888`; `scsp2.c:2818` |
| | 8 | `DAC18B` | 18-bit DAC output flag | R/W, **inert** | stored only in `scsp2.c:2819`; **not decoded at all in `scsp.c`** |
| | 7:4 | `VER` | Hardware version, hardwired 0 | R only | `scsp.c:3013-3014` (read masks to `0x0F`); `scsp2.c:2816-2817` (`SCSP_VERSION 0`, `scsp2.c:126`) |
| | 3:0 | `MVOL` | Master volume, 0 = silent … 15 = full | R/W | `scsp.c:2733`, `2889`; `scsp2.c:2820` |
| `$402` | 8:7 | `RBL` | DSP ring-buffer length: `0x2000 << RBL` words | R/W | `scsp.c:2737`/`2741`/`2904`; `scsp2.c:2837` |
| | 6:0 | `RBP` | DSP ring-buffer base, = bits 19:13 of a sound-RAM byte address | R/W | `scsp.c:2742-2745`, `2906-2909`; `scsp2.c:2838` |
| `$404` | 12 | `MOFULL` | MIDI out FIFO full | R only | `scsp.c:1602` (`SCSP_MIDI_OUT_FUL 0x10`), `3017-3018`; `scsp2.c:3127` |
| | 11 | `MOEMP` | MIDI out FIFO empty | R only | `scsp.c:1601` (`0x08`); `scsp2.c:3128` |
| | 10 | `MIOVF` | MIDI in FIFO overflow | R only | `scsp.c:1600` (`0x04`); `scsp2.c:3129` |
| | 9 | `MIFULL` | MIDI in FIFO full | R only | `scsp.c:1599` (`0x02`); `scsp2.c:3130` |
| | 8 | `MIEMP` | MIDI in FIFO empty | R only | `scsp.c:1598` (`0x01`); `scsp2.c:3131` |
| | 7:0 | `MIBUF` | MIDI in data (reading pops the FIFO) | R only | `scsp.c:3020-3021`, `3070-3075`; `scsp2.c:3109-3133` |
| `$406` | 7:0 | `MOBUF` | MIDI out data (writing pushes the FIFO) | R/W | `scsp.c:2748-2749`, `2913-2914`, `3023-3024`; `scsp2.c:2844-2847` |
| `$408` | 15:11 | `MSLC` | Monitor slot select (0-31) | W | `scsp.c:2752-2754`, `2917-2919`; `scsp2.c:2850-2851` |
| | 10:7 | `CA` | Call address = top 4 bits of monitored slot's playback position | R only | `scsp.c:3081`, `3948`/`3955`; `scsp2.c:2430`, `2449` |
| | 6:5 | `SGC` | Envelope phase of monitored slot | R only | `scsp.c:3949`/`3956`; `scsp2.c:2432-2445` |
| | 4:0 | `EG` | Envelope level of monitored slot | R only | `scsp.c:3950`/`3957`; `scsp2.c:2447` |
| `$40A-$410` | — | — | Not decoded anywhere | — | `scsp.c:65-68`, `scsp2.c:371` |
| `$412` | 15:1 | `DMEAL` | DMA sound-RAM address, bits 15:1 | R/W | `scsp.c:2757-2762`, `2922-2923`; `scsp2.c:2854-2856` |
| `$414` | 15:12 | `DMEAH` | DMA sound-RAM address, bits 19:16 | R/W | `scsp2.c:2861`; **`scsp.c` only implements bits 18:16** (`:2766`, `:2927`, mask `0x70`/`0x7000`) |
| | 11:1 | `DRGA` | DMA register-block address, bits 11:1 | R/W | `scsp.c:2767`, `2771`, `2928`; `scsp2.c:2862` |
| `$416` | 14 | `DGATE` | Zero-fill instead of copy | R/W | `scsp.c:2776` (`dmfl & 0x40`), `2933`; `scsp2.c:2867` |
| | 13 | `DDIR` | Transfer direction | R/W | `scsp.c:2776` (`dmfl & 0x20`); `scsp2.c:2868` — **polarity differs between the two, see §8** |
| | 12 | `DEXE` | Write 1 to start; cleared when done | R/W | `scsp.c:2776`, `1991-1992`; `scsp2.c:2869`, `3188-3189` |
| | 11:1 | `DTLG` | Transfer byte count, bits 11:1 | R/W | `scsp.c:2775`, `2780`, `2932`; `scsp2.c:2870` |
| `$418` | 10:8 | `TACTL` | Timer A prescaler | R/W | `scsp.c:2784`, `2937`, `3084`; `scsp2.c:2877` |
| | 7:0 | `TIMA` | Timer A count (8.8 fixed point internally) | R/W | `scsp.c:2788`, `2938`; `scsp2.c:2878` |
| `$41A` | 10:8 / 7:0 | `TBCTL` / `TIMB` | Timer B | R/W | `scsp.c:2792`, `2796`, `2942-2943`; `scsp2.c:2883-2884` |
| `$41C` | 10:8 / 7:0 | `TCCTL` / `TIMC` | Timer C | R/W | `scsp.c:2800`, `2804`, `2947-2948`; `scsp2.c:2889-2890` |
| `$41E` | 10:0 | `SCIEB` | Sound-CPU (M68K) interrupt enable | R/W | `scsp.c:2810`, `2817`, `2953`, `3032-3036`, `3092`; `scsp2.c:2894-2899` |
| `$420` | 10:0 | `SCIPD` | Sound-CPU interrupt pending | R; W only bit 5 | `scsp.c:2822`, `2958`, `3038-3042`, `3095`; `scsp2.c:2902-2905` |
| `$422` | 10:0 | `SCIRE` | Sound-CPU interrupt reset (write 1 to clear) | W | `scsp.c:2826`, `2831`, `2962`; `scsp2.c:2907-2909` |
| `$424` | 7:0 | `SCILV0` | M68K IPL bit 0 per interrupt source | R/W | `scsp.c:2836`, `2966`; `scsp2.c:2911-2913` |
| `$426` | 7:0 | `SCILV1` | M68K IPL bit 1 | R/W | `scsp.c:2841`, `2970`; `scsp2.c:2916-2918` |
| `$428` | 7:0 | `SCILV2` | M68K IPL bit 2 | R/W | `scsp.c:2846`, `2974`; `scsp2.c:2921-2923` |
| `$42A` | 10:0 | `MCIEB` | Main-CPU (SCU) interrupt enable | R/W | `scsp.c:2851`, `2855`, `2981`; `scsp2.c:2926-2930` |
| `$42C` | 10:0 | `MCIPD` | Main-CPU interrupt pending | R; W only bit 5 | `scsp.c:2859`, `2991`, `3044-3048`, `3101`; `scsp2.c:2933-2936` |
| `$42E` | 10:0 | `MCIRE` | Main-CPU interrupt reset | W | `scsp.c:2864`, `2868`, `2995`; `scsp2.c:2938-2940` |

### 2.2 `MEM4MB` and its side effects

Writing `$400` reconfigures the M68K instruction-fetch banks immediately
(`scsp.c:2719-2729`, `2890-2900`; `scsp2.c:2822-2831`):

```c
if (scsp.mem4b) M68K->SetFetch(0x000000, 0x080000, (pointer)SoundRam);
else { M68K->SetFetch(0x000000, 0x040000, SoundRam);
       M68K->SetFetch(0x040000, 0x080000, SoundRam);
       M68K->SetFetch(0x080000, 0x0C0000, SoundRam);
       M68K->SetFetch(0x0C0000, 0x100000, SoundRam); }
```

`C68k_Set_Fetch` (`yabause/src/c68k/c68k.c:217-225`) rebases each 64 KB bank so that the four
calls make sound RAM alias every 256 KB across the whole first megabyte; the single `mem4b`
call maps only the first 512 KB and **leaves banks above `0x090000` holding stale pointers from
the previous configuration** — **[BUG]**, latent because the data path (below) returns 0 there.

`scsp2.c` additionally derives a mask, `scsp.sound_ram_mask = mem4mb ? 0x7FFFF : 0x3FFFF`
(`scsp2.c:2831`), used uniformly by every sound-RAM accessor. `scsp.c` open-codes the same test
in each accessor (`scsp.c:4875-4878`, `4950-4953`, `4973-4976`, `4993-4999`, `5046-5049`).

### 2.3 `RBP` is stored differently by each engine

```c
case 0x03: // RBL(low bit)/RBP
   scsp.rbl = (scsp.rbl & 2) + ((d >> 7) & 1);
   if (use_new_scsp) scsp.rbp = (d & 0x7F);
   else              scsp.rbp = (d & 0x7F) * (4 * 1024 * 2);
```

(`scsp.c:2740-2746`, mirrored at `2903-2911`.) The new engine hands the raw 7-bit value to the
DSP, which shifts it itself (`scspdsp.c:271`: `(addr + (dsp->rbp << 12)) & 0x3FFFF`, in **word**
units — equivalent to `rbp * 0x2000` bytes). The old engine pre-multiplies by `0x2000` bytes but
never uses the result for anything, since it has no DSP. `scsp2.c:2838` stores
`((data >> 0) & 0x7F) << 13` (a byte address) and likewise never uses it.

### 2.4 The `$408` monitor register

`scsp_update_monitor()` (`scsp.c:3943-3964`) recomputes `CA`/`SGC`/`EG` from the slot selected
by `MSLC`. It is called on every `MSLC` write (`scsp.c:2754`, `2919`), once per generated sample
in the new engine (`scsp.c:5345`), and once per frame in the old engine (`scsp.c:5845-5846`).

| | old engine (`scsp.c:3954-3957`) | new engine (`scsp.c:3948-3950`) | `scsp2.c:2426-2450` |
|---|---|---|---|
| `CA` | `((fcnt >> (SCSP_FREQ_LB+12)) & 0xF) << 7` | `sample_offset >> 5` (**not** shifted into place) | `(addr_counter >> (FREQ_LOW_BITS+12)) & 0xF`, then `<< 7` at read |
| `SGC` | `ecurp` (0=attack,1=decay,2=sustain,3=release, `scsp.c:1604-1607`) | `state.envelope` (0=ATTACK,1=DECAY1,2=DECAY2,3=RELEASE, `scsp.c:186-192`) | remapped explicitly to 0/1/2/3 |
| `EG` | `0x1f - (env >> (SCSP_ENV_HB-5))` — *inverted* (env is a loudness multiplier) | `attenuation >> 5` — *not inverted* (attenuation is already 0=loud) | `0x1f - (last_env >> 27)` |

The word read is `(scsp.ca & 0x780) | (sgc << 5) | eg` (`scsp.c:3081`); the byte read at `$408`
returns `scsp.ca >> 8` and at `$409` `(scsp.ca & 0xE0) | (sgc << 5) | eg` (`scsp.c:3026-3030`).
**[BUG]** three separate problems: (a) the byte and word reads disagree about which bits of `ca`
are the CA field (`0xE0` vs `0x780`); (b) in the `$409` byte read `ca & 0xE0` occupies bits 7:5
and therefore *collides* with `sgc << 5` at bits 6:5 — the commented-out line immediately above
(`scsp.c:3009`) shows the intent was `((fcnt >> (SCSP_FREQ_LB + 12)) & 0x1) << 7`, a single CA
bit in bit 7; (c) in the new engine `ca` is never shifted left by 7, so the word read
`(ca & 0x780)` actually surfaces bits 15:12 of the raw sample offset. The comment at `scsp.c:3857-3858`
(*"Still not correct, but at least this fixes games that rely on Call Address information"*)
confirms this register is known-approximate.

### 2.5 Reset values

`scsp_reset()` (`scsp.c:4551-4605`): all of `mem4b`, `mvol`, `rbl`, `rbp`, `mslc`, `ca`, `dmea`,
`drga`, `dmfl`, `dmlen`, `mcieb`, `mcipd`, `scieb`, `scipd`, `scilv0/1/2` → 0;
`timacnt = timbcnt = timccnt = 0xFF00` (i.e. *expired*); `timasd = timbsd = timcsd = 0`;
`midflag = SCSP_MIDI_IN_EMP | SCSP_MIDI_OUT_EMP`; the whole `scsp_reg[0x1000]` shadow zeroed.
Every slot: `ecnt = SCSP_ENV_DE` (off), `ecurp = SCSP_ENV_RELEASE`,
`dislr = disll = efslr = efsll = 31` (muted), `lfofmw/lfoemw` pointed at the sawtooth tables so
they are never NULL.

`new_scsp_reset()` (`scsp.c:1558-1577`): zeroes the whole `struct Scsp`, then per slot sets
`attenuation = 0x3FF`, `envelope = RELEASE`, `num = slot_num`; regenerates the PLFO/ALFO tables;
zeroes the entire `ScspDsp`.

`ScspReset()` in `scsp2.c:983-1060` matches, plus `moemp = miemp = 1`,
`scsp_regcache[0x400>>1] = SCSP_VERSION << 4`, `sound_ram_mask = 0x3FFFF`,
`outshift_l = outshift_r = 31`, `audiogen = audiogen_null`.

---

## 3. Individual Slot Registers (ISR), `$000-$3FF`

32 slots, `$20` bytes apiece. Slot number = `(addr >> 5) & 0x1F` (`scsp.c:1024`, `1146`, `1259`,
`1348`; `scsp2.c:2602`). Offsets `$18-$1E` are not decoded by anything. The author's map is at
`scsp.c:87-102`.

### 3.1 Complete bit-field table

| Off | Bits | Name | Width | Meaning | New-engine decode | Old-engine decode | `scsp2.c` |
|---|---|---|---|---|---|---|---|
| `$00` | 12 | `KYONEX` | 1 | Write 1 → apply `KYONB` of **all 32 slots** at once. Not stored. | `:1035`, `:1270` | `:2164`, `:2468` | `:2618`, `:2621` |
| | 11 | `KYONB` | 1 | This slot's desired key state | `:1033`, `:1268` | `:2160`, `:2456` | `:2607` |
| | 10:9 | `SBCTL` | 2 | Source bit control (0 none, 1 invert data bits, 2 invert sign, 3 both — per `scsp.c:6783-6798`) | `:1040`, `:1275` | `:2161`, `:2457` | `:2608` |
| | 8:7 | `SSCTL` | 2 | Sound source: 0 external DRAM, 1 internal noise, 2 internal zeros, 3 invalid (`scsp.c:6766-6781`) | `:1041`/`:1044`, `:1276` | `:2162`/`:2171`, `:2458` | `:2609` |
| | 6:5 | `LPCTL` | 2 | Loop mode: 0 off, 1 normal, 2 reverse, 3 alternating (ping-pong) (`scsp.c:6801-6816`) | `:1045`, `:1277` | `:2172`, `:2459` | `:2610` |
| | 4 | `PCM8B` | 1 | 1 = 8-bit samples, 0 = 16-bit | `:1046`, `:1278` | `:2174`, `:2461` | `:2611` |
| | 3:0 | `SA[19:16]` | 4 | Start address, high nibble (**byte** address into sound RAM) | `:1047`, `:1279` | `:2175`, `:2462` | `:2612` |
| `$02` | 15:0 | `SA[15:0]` | 16 | Start address, low word | `:1050`, `:1053`, `:1282` | `:2183`, `:2190`, `:2473` | `:2625` |
| `$04` | 15:0 | `LSA` | 16 | Loop start address, in **samples** relative to SA | `:1056`, `:1059`, `:1285` | `:2197`, `:2202`, `:2481` | `:2631` |
| `$06` | 15:0 | `LEA` | 16 | Loop end address, in samples relative to SA | `:1062`, `:1065`, `:1288` | `:2207`, `:2212`, `:2485` | `:2637` |
| `$08` | 15:11 | `D2R` | 5 | Decay-2 (sustain) rate | `:1068`, `:1291` | `:2217`, `:2489` (`slot->sr`) | `:2643` (`sr`) |
| | 10:6 | `D1R` | 5 | Decay-1 rate | `:1069`/`:1072`, `:1292` | `:2218`/`:2234`, `:2490` (`dr`) | `:2644` (`dr`) |
| | 5 | `EGHOLD` | 1 | Envelope hold | `:1073`, `:1293` (`hold`) | `:2235`, `:2491` | `:2645` |
| | 4:0 | `AR` | 5 | Attack rate | `:1074`, `:1294` | `:2236`, `:2492` | `:2646` |
| `$0A` | 15 | — | 1 | Undocumented; stored as `unknown1` by the new engine only | `:1079`, `:1299` | not stored | not stored (masked off, `:2651`) |
| | 14 | `LPSLNK` | 1 | Loop start link — begin D1R when LSA is reached | `:1080`, `:1300` (`ls`) | `:2252`, `:2513` (`lslnk`) | `:2652` (`lpslnk`) |
| | 13:10 | `KRS` | 4 | Key rate scaling; `0xF` = scaling disabled | `:1081`, `:1301` | `:2253-2258`, `:2514-2519` | `:2653`, `:2657-2660` |
| | 9:5 | `DL` | 5 | Decay level (attack→D2 threshold) | `:1082`/`:1085`, `:1302` | `:2260-2262`, `:2521` (`sl`) | `:2654` (`sl`) |
| | 4:0 | `RR` | 5 | Release rate | `:1086`, `:1303` | `:2271`, `:2522` | `:2655` |
| `$0C` | 11:10 | — | 2 | Undocumented; stored as `unknown2` by the new engine only | `:1089`, `:1306` | not stored | masked off (`:2670`) |
| | 9 | `STWINH` | 1 | Stack write inhibit | `:1090`, `:1307` (`si`) | `:2282`, `:2533` — **stored into `sdir`, see [BUG] below** | `:2671` |
| | 8 | `SDIR` | 1 | Sound direct (bypass EG/TL) | `:1091`, `:1308` (`sd`) | `:2283`, `:2534` — **stored into `swe`** | `:2672` |
| | 7:0 | `TL` | 8 | Total level (attenuation) | `:1094`, `:1309` | `:2287`, `:2535` | `:2673` |
| `$0E` | 15:12 | `MDL` | 4 | Modulation level | `:1097`, `:1312` | `:2291`, `:2539` | `:2680` |
| | 11:6 | `MDXSL` | 6 | Modulation input X — sound-stack slot select | `:1098`/`:1101`, `:1313` | `:2292`/`:2296`, `:2540` | `:2681` |
| | 5:0 | `MDYSL` | 6 | Modulation input Y | `:1102`, `:1314` | `:2297`, `:2541` | `:2682` |
| `$10` | 15 | — | 1 | Undocumented; `unknown3` | `:1105`, `:1317` | not stored | masked off (`:2686`) |
| | 14:11 | `OCT` | 4 | Octave, **signed** −8…+7 | `:1106`, `:1319` | `:2301-2304`, `:2545-2548` | `:2687`, `:2690-2693` |
| | 10 | — | 1 | Undocumented; `unknown4` | `:1107`, `:1318` | not stored | masked off |
| | 9:0 | `FNS` | 10 | Frequency number switch (pitch fraction) | `:1108`, `:1111`, `:1320` | `:2306`, `:2313`, `:2550` | `:2688` |
| `$12` | 15 | `LFORE` | 1 | LFO reset / hold | `:1114`, `:1323` (`re`) | `:2318-2326`, `:2556-2564` | `:2701`, `:2708-2714` |
| | 14:10 | `LFOF` | 5 | LFO frequency index (0 slowest … 31 fastest) | `:1115`, `:1324` | `:2328`, `:2566` | `:2702` |
| | 9:8 | `PLFOWS` | 2 | Pitch-LFO waveform: 0 saw, 1 square, 2 triangle, 3 noise | `:1116`, `:1325` | `:2330-2347`, `:2577-2594` | `:2703` |
| | 7:5 | `PLFOS` | 3 | Pitch-LFO sensitivity; 0 = off | `:1119`, `:1326` | `:2350-2353`, `:2567-2570` | `:2704`, `:2718-2721` |
| | 4:3 | `ALFOWS` | 2 | Amplitude-LFO waveform (same encoding) | `:1120`, `:1327` | `:2360-2376`, `:2596-2612` | `:2705` |
| | 2:0 | `ALFOS` | 3 | Amplitude-LFO sensitivity; 0 = off | `:1121`, `:1328` | `:2355-2358`, `:2572-2575` | `:2706`, `:2722-2725` |
| `$14` | 7 | — | 1 | Undocumented; `unknown5` | `:1127`, `:1331` | not stored | masked off (`:2735`) |
| | 6:3 | `ISEL` | 4 | DSP `MIXS` input select (which of 16 mix inputs this slot feeds) | `:1128`, `:1332` | **never stored** | `:2736` |
| | 2:0 | `IMXL` | 3 | Input mix level into `MIXS`; 0 = off | `:1129`, `:1333` | `:2380-2383`, `:2616-2619` | `:2737`, `:2739-2742` |
| `$16` | 15:13 | `DISDL` | 3 | Direct send level; 0 = off, 7 = 0 dB, 6 dB/step (`scsp.c:6744-6757`) | `:1132`, `:1336` | `:2387-2412`, `:2623-2648` | `:2747`, `:2761-2778` |
| | 12:8 | `DIPAN` | 5 | Direct pan; bit 4 = pan-left flag, bits 3:0 = magnitude | `:1133`, `:1337` | as above | `:2748` |
| | 7:5 | `EFSDL` | 3 | Effect (DSP) send level | `:1136`, `:1338` | `:2416-2440`, `:2650-2673` | `:2749`, `:2780-2797` |
| | 4:0 | `EFPAN` | 5 | Effect pan | `:1137`, `:1339` | as above | `:2750` |
| `$18-$1E` | — | — | — | Not decoded | `default: break` `:1139`, `:1341` | `switch` has no case | `goto unhandled_write` `:2806` |

**[BUG] `STWINH`/`SDIR` are swapped in the old engine.** `scsp_slot_set_b` case `0x0C`
(`scsp.c:2281-2284`) does `slot->sdir = d & 2; slot->swe = d & 1;` on the *high* byte of `$0C`,
i.e. it puts bit 9 (STWINH) into the variable named `sdir` and bit 8 (SDIR) into the variable
named `swe` ("stack write enable"). The word path (`scsp.c:2532-2535`) repeats the swap.
Harmless in practice because neither variable is ever read by the synthesis code — but it makes
`ScspSlotDebugStats` print "Stack Write Inhibited" and "Sound Direct Enabled" the wrong way
round (`scsp.c:6847-6851`).

**[QUIRK] Fields that are decoded but never used by any engine:** `SBCTL`, `STWINH`/`SDIR`,
`LPSLNK`. Grep confirms they appear only in the register decode, the savestate, and the debug
printer. `SSCTL` is used only as a "silence this slot" flag (§4.5). `ISEL` is used only by the
new engine.

### 3.2 Read-back semantics

- **New engine** (`scsp_slot_read_byte` `:1144`, `scsp_slot_read_word` `:1346`) reconstructs the
  word from the stored fields. `KYONEX` is never stored and reads back as 0 (`scsp.c:1356`).
  The `kx` field in `SlotRegs` (`scsp.c:273`) is **never assigned** — dead, but still written to
  savestates (`scsp.c:6100`).
- **Old engine** (`scsp_slot_get_b` `:2678`, `scsp_slot_get_w` `:2691`) returns the raw
  `scsp_isr[]` shadow, masking out `KYONEX`: `val &= 0xEF` for byte `$x0`, `val &= 0xEFFF` for
  word `$x0`.
- **`scsp2.c`** returns `scsp_regcache[]` with `KYONEX` stripped at write time
  (`data &= 0x0FFF`, `scsp2.c:2621`).
- **[QUIRK]** In the new engine, `scsp_w_b`/`scsp_w_w` write *both* the shadow and the decoded
  fields (`scsp.c:4140-4141`, `4254-4255`), but reads come only from the decoded fields — so
  writes to the undecoded offsets `$18-$1E` are silently lost on read-back, whereas the old
  engine preserves them.

### 3.3 Key-on / key-off

`KYONEX` is a global trigger, not a per-slot one:

```c
void keyonex(struct Scsp *s) {
   for (channel = 0; channel < 32; channel++) {
     if (s->slots[channel].regs.kb) keyon(&s->slots[channel]);
     else                           keyoff(&s->slots[channel]);
   }
}
```

(`scsp.c:1001-1020`; old engine `scsp_slot_keyonoff` `:2055-2067`; `scsp2.c:2962-2972`.)
`KYONB` must be written *before* the `KYONEX` bit is acted on, which both byte and word paths
guarantee by ordering (`scsp.c:1033` with the comment "has to be done first", `:1268`).

`keyon` (new engine, `scsp.c:929-994`) is a no-op unless the slot is currently in `RELEASE`.
When it fires:

| Field | Value | Line |
|---|---|---|
| `envelope` | `ATTACK` | `:933` |
| `attenuation` | `0x280` (**not** `0x3FF`) | `:934` |
| `sample_counter` | 0 | `:935` |
| `step_count` | 0 | `:936` |
| `sample_offset` | 0 | `:937` |
| `envelope_steps_taken` | 0 | `:938` |
| `regs.sa` | forced even if `!PCM8B` (`sa &= 0xFFFFFE`) | `:940-942` |

**[QUIRK]** the `sa` alignment fix-up masks with `0xFFFFFE` — a **24-bit** mask applied to a
20-bit address. Works, but is inconsistent with the `0xFFFFF`/`0x7FFFF` masks used everywhere
else. `scsp2.c:3021` uses the cleaner `slot->sa &= ~1`.

`keyoff` (`scsp.c:996-999`) simply does `change_envelope_state(slot, RELEASE)` — it resets
`step_count` to 0 but leaves `attenuation` alone.

Old engine `scsp_slot_keyon` (`scsp.c:2001-2035`) additionally recomputes the sample-data
pointer `buf8`/`buf16` and *clamps `lea` to the end of sound RAM*, then resets `fcnt = 0`,
`ecnt = SCSP_ENV_AS`, `env = 0`, `einc = &einca`, `ecurp = ATTACK`, `ecmp = SCSP_ENV_AE`,
`enxt = scsp_attack_next`. Its own comment flags the envelope reset as suspect:
`// reset envelope counter (probably wrong, should convert decay to attack?)` (`scsp.c:2026-2027`).

Old-engine `scsp_slot_keyoff` (`scsp.c:2037-2053`) does convert attack to release properly:
`if (ecurp == ATTACK) ecnt = SCSP_ENV_DE - ecnt;`. `scsp2.c:3002-3003` does the same. The new
engine does **not** — **[QUIRK]**, a key-off during attack leaves the attenuation where it was.

**[QUIRK] Re-triggering a finished one-shot.** When a non-looping sample runs off the end, the
new engine forces `attenuation = 0x3FF` (`scsp.c:564`) but leaves `envelope` at whatever phase
it was in. Since `keyon` requires `envelope == RELEASE`, such a slot cannot be re-keyed until
something drives a key-off first. In practice `keyonex` supplies that, because it calls
`keyoff` for every slot whose `KYONB` is clear.

---

## 4. The new engine's synthesis pipeline

`generate_sample()` (`scsp.c:1450-1556`) produces exactly one stereo output sample per call by
running 32 pipeline steps:

```c
// run 32 steps to generate 1 full sample (512 clock cycles at 22579200hz)
// 7 operations happen simultaneously on different channels due to pipelining
for (step_num = 0; step_num < 32; step_num++) {
   op1(&s->slots[step_num]);                    // phase, pitch LFO
   op2(&s->slots[(step_num-1) & 0x1f], s);      // address pointer, modulation read
   op3(&s->slots[(step_num-2) & 0x1f]);         // waveform DRAM read
   op4(&s->slots[(step_num-3) & 0x1f]);         // interpolation, EG
   op5(&s->slots[(step_num-4) & 0x1f]);         // level calc 1 (TL + amplitude LFO)
   op6(&s->slots[(step_num-5) & 0x1f]);         // level calc 2  -- EMPTY
   op7(&s->slots[(step_num-6) & 0x1f], s);      // sound stack write
   ...
}
```

`new_scsp_exec_real` (`scsp.c:5349-5358`) calls `new_scsp_run_sample()` (`scsp.c:5312-5347`)
every 512 cycles of its input clock, and `new_scsp_run_sample` calls `scsp_update_timer(1)` then
`generate_sample`.

Every stage except `op7` early-returns when `attenuation >= 0x3bf` (`scsp.c:498`, `537`, `632`,
`737`, `791`) — that is the "effectively silent, stop working" threshold. `op5` additionally
forces `output = 0` in that case.

### 4.1 `op1` — phase accumulator and pitch LFO (`scsp.c:490-523`)

```c
u32 oct = slot->regs.oct ^ 8;          // map signed OCT (-8..7) to 0..15
u32 fns = slot->regs.fns ^ 0x400;      // FNS is 10 bits, so this ORs in the implicit 1
u32 phase_increment = fns << oct;
...
plfo_shifted = (plfo_val << slot->regs.plfos) >> 2;
slot->state.waveform_phase_value &= (1 << 18) - 1;      // keep only the fraction
slot->state.waveform_phase_value += (phase_increment + plfo_shifted);
```

The phase accumulator has **18 fractional bits**. `OCT = 0` gives `oct^8 == 8` and
`(0x400 + FNS) << 8`, so `FNS = 0` at `OCT = 0` advances exactly `0x40000 = 2^18` — one sample
per step. Each octave step doubles or halves it. `op2` reads the whole part with
`sample_delta = waveform_phase_value >> 18` (`scsp.c:535`) and `op1` masks the fraction back
down before the next add, so the delta is consumed exactly once.

LFO position advance (`scsp.c:501-508`):

```c
if (slot->state.lfo_counter % lfo_step_table[slot->regs.lfof] == 0) {
   slot->state.lfo_counter = 0;
   slot->state.lfo_pos++;
   if (slot->state.lfo_pos > 0xff) slot->state.lfo_pos = 0;
}
```

`lfo_counter` is incremented once per sample in `op7`. The LFO tables are 256 entries, so the
LFO period is `256 * lfo_step_table[LFOF]` samples.

**[QUIRK] `PLFOS = 0` does not disable pitch modulation.** `plfo_shifted = (plfo_val << 0) >> 2`
is non-zero. Both the old engine (`scsp.c:3890`, table index `(lfofms == 31) ? 0 : 1`) and
`scsp2.c` (`lfo_fm_shift = -1`, `scsp2.c:2721`) correctly treat `PLFOS = 0` as "off".

### 4.2 `op2` — address pointer, loop control, modulation (`scsp.c:532-624`)

Modulation (only implemented here, in the whole tree):

```c
if (slot->regs.mdl) {
   u32 x_sel = get_slot(slot, slot->regs.mdxsl);   // (mdxsl + slot_num) & 0x1f
   u32 y_sel = get_slot(slot, slot->regs.mdysl);
   s16 xd = s->sound_stack[x_sel];
   s16 yd = s->sound_stack[y_sel];
   s32 zd = (xd + yd) / 2;                          // averaging operation
   u16 shift = 0xf - (slot->regs.mdl);
   zd >>= shift;
   md_out = zd;
}
```

So `MDL = 0` means no modulation, `MDL = 15` means no shift (maximum depth), and the modulation
value is added to the *sample offset*, not the phase.

**[QUIRK]** `get_slot()` (`scsp.c:525-528`) masks the 6-bit `MDXSL`/`MDYSL` to 5 bits, so only
sound-stack entries 0-31 — the *previous* generation — are reachable. Entries 32-63 (the current
generation) can never be selected, even though the register field and the stack are both 64
entries wide.

Loop handling, exactly as coded:

| `LPCTL` | Behaviour (`scsp.c:559-618`) |
|---|---|
| 0 (off) | `sample_offset += delta`; when `>= LEA`, force `attenuation = 0x3FF` (slot dies) |
| 1 (normal) | `sample_offset += delta`; when `>= LEA`, `sample_offset = LSA` |
| 2 (reverse) | forwards until `>= LEA`, then clamp to `LEA`, set `backwards`; thereafter `-= delta`, and when `<= LSA` jump back to `LEA` |
| 3 (ping-pong) | forwards until `>= LEA` → clamp to `LEA`, `backwards = 1`; backwards until `<= LSA` → clamp to `LSA`, `backwards = 0` |

Final address (`scsp.c:620-623`):

```c
if (!pcm8b) address_pointer = (s32)sa + (sample_offset + md_out) * 2;
else        address_pointer = (s32)sa + (sample_offset + md_out);
```

so `SA` is a **byte** address and `LSA`/`LEA`/`sample_offset` are **sample** counts.

**[QUIRK]** In mode 1 the wrap is `sample_offset = LSA` — a hard reset, not a modulo — so any
overshoot past `LEA` is discarded and the loop is up to one phase-step short. `scsp2.c:679-681`
does it properly with `lsa_shifted + ((addr_counter - lsa_shifted) % looplen_shifted)`; the old
engine (`SCSP_UPDATE_PHASE`, `scsp.c:3180-3192`) also does the hard reset.

**[QUIRK]** In mode 2 there is no lower clamp on `sample_offset` before the comparison, and the
comparison is `<=` against `LSA`, so a large negative `delta` can push the offset far below
`LSA` in one step and it will be snapped to `LEA` — no interpolation, no partial step.

### 4.3 `op3` — waveform read (`scsp.c:628-641`)

```c
u32 addr = (slot->state.address_pointer) & 0x7FFFF;
if (!slot->regs.pcm8b) slot->state.wave = T2ReadWord(SoundRam, addr);
else                   slot->state.wave = T2ReadByte(SoundRam, addr) << 8;
slot->state.output = slot->state.wave;
```

**[QUIRK]** the mask is always `0x7FFFF` (512 KB) regardless of `MEM4MB`, and the commented-out
call to `SoundRamReadWord(addr)` shows the mirroring path was deliberately bypassed for speed.
**[QUIRK]** `wave` is `u16` and `output` is `s16`; there is no sign handling, no `SBCTL`
bit-reversal, and no interpolation despite `op4`'s comment ("interpolation, eg").

### 4.4 Output mixing, panning and master volume (`scsp.c:1479-1555`)

Per pipeline step, for the slot that has just left the pipeline (`last_step = (step_num-6) & 0x1f`):

```c
int disdl = get_sdl_shift(s->slots[last_step].regs.disdl);
s16 disdl_applied = (s->slots[last_step].state.output >> disdl);
s16 mixs_input = s->slots[last_step].state.output >> get_sdl_shift(s->slots[last_step].regs.imxl);
get_panning(s->slots[last_step].regs.dipan, &pan_val_l, &pan_val_r);
outl32 += ((disdl_applied >> pan_val_l) >> 1);
outr32 += ((disdl_applied >> pan_val_r) >> 1);
scsp_dsp.mixs[s->slots[last_step].regs.isel] += mixs_input << 4;
```

with

```c
int get_sdl_shift(int sdl) { if (sdl == 0) return 16; else return (7 - sdl); }   // :1441-1446
void get_panning(int pan, int *l, int *r) {                                      // :1426-1439
   if (pan & 0x10) { *l = 0; *r = pan & 0xf; }   // bit 4 set = pan left, attenuate right
   else            { *l = pan & 0xf; *r = 0; }
}
```

So send levels are 6 dB per step (`DISDL`/`EFSDL`/`IMXL`: 7 = 0 dB, 1 = −36 dB, 0 = silent —
consistent with `AddSoundLevel`'s `-(7-level)*6` dB at `scsp.c:6753`).

**[QUIRK] Panning resolution.** The new engine uses the 4-bit pan magnitude directly as a shift
count → **6 dB per pan step**, up to 90 dB. The old engine uses `+= (d >> 1) & 7`
(`scsp.c:2398`, `2406`, `2425`, `2433`) → ~3 dB per step but with the LSB thrown away; `scsp2.c`
does the same and says so explicitly: *"we lose 1 bit of resolution from the panning parameter
because we adjust the output level by shifting (powers of two), while DIPAN/EFPAN have a
resolution of sqrt(2)"* (`scsp2.c:2756-2759`). The debug printer's `AddSoundPan`
(`scsp.c:6723-6740`) uses 3 dB per step, corroborating that 3 dB is the intended hardware step
and that the new engine's panning is twice as steep as it should be.

**[BUG]** `AddSoundPan` also disagrees with `get_panning` about direction: for `pan & 0x10` it
prints `Left = -(pan&0xF)*3 dB`, i.e. it attenuates the *left*, while every decoder attenuates
the right. Cosmetic only.

DSP effect return, master volume and clipping (`scsp.c:1526-1555`):

```c
for (i = 0; i < 18; i++) {          // 16,17 are exts0/1
   int efsdl = get_sdl_shift(s->slots[i].regs.efsdl);
   if (i < 16)       efsdl_applied = (scsp_dsp.efreg[i] >> efsdl);
   else if (i == 16) efsdl_applied = scsp_dsp.exts[0] >> efsdl;
   else              efsdl_applied = scsp_dsp.exts[1] >> efsdl;
   get_panning(s->slots[i].regs.efpan, &pan_val_l, &pan_val_r);
   outl32 += (efsdl_applied >> pan_val_l) >> 1;
   outr32 += (efsdl_applied >> pan_val_r) >> 1;
}
mvol_shift = 0xf - mvol;
outl32 >>= mvol_shift;  *out_l = min(SHRT_MAX, max(SHRT_MIN, outl32));
outr32 >>= mvol_shift;  *out_r = min(SHRT_MAX, max(SHRT_MIN, outr32));
```

i.e. the 16 DSP effect outputs `EFREG[0..15]` are panned/attenuated by *slots 0-15*'
`EFSDL`/`EFPAN`, and the two CD inputs `EXTS0/1` by slots 16 and 17's. Master volume is a shift:
`MVOL = 15` → no attenuation, each step down is −6 dB.

### 4.5 What the old engine does instead

`scsp_update(bufL, bufR, len)` (`scsp.c:3843-3941`) walks the 32 slots and dispatches each
through a 5-dimensional function-pointer table
`scsp_slot_update_p[FMS][EMS][16BIT][LEFT][RIGHT]` (`scsp.c:3715-3841`, 32 generated variants
built from the macro set at `scsp.c:3117-3220`). Notable behaviours:

- Slots with `ecnt >= SCSP_ENV_DE` are skipped entirely (`scsp.c:3853`).
- **[QUIRK]** Slots with `SSCTL != 0` are silenced; only the phase counter is advanced, "so that
  games that rely on Call Address information" still work (`scsp.c:3855-3876`). Noise and
  zero-source generation are *not implemented*. `scsp2.c:3091-3093` does the same.
- **[QUIRK] The effect send is repurposed as a direct send:** `if ((disll == 31) && (dislr ==
  31)) { disll = efsll; dislr = efslr; }` (`scsp.c:3881-3886`, and identically in
  `scsp2.c:2780-2798`). Since there is no DSP in either, a slot routed only to the effect bus
  is played dry at the effect level instead of being dropped. This is an explicit substitute for
  the missing DSP.
- **[QUIRK] MVOL is never applied.** Neither `scsp_update` nor anything in `scsp2.c` reads
  `scsp.mvol`.
- Output is `out * env >> disll` / `>> dislr`, accumulated into `s32` buffers with **no clipping
  at the mix stage** (`scsp.c:3158-3178`); saturation happens only in `ScspConvert32uto16s`
  (`scsp.c:5443-5472`) or in the host backend.

---

## 5. Envelope generator

There are three different envelope models in the source set.

### 5.1 New engine: attenuation register + modulus tables

State is a single 10-bit `attenuation` (`scsp.c:323`), 0 = full volume, `0x3FF` = silence, plus
the phase enum `ATTACK / DECAY1 / DECAY2 / RELEASE` (`scsp.c:186-192`).

**Effective rate** — `get_rate()` (`scsp.c:692-711`):

```c
if (slot->regs.krs == 0xf) result = rate * 2;
else {
   result = (slot->regs.krs * 2) + (rate * 2) + ((slot->regs.fns >> 9) & 1);
   result = (8 ^ slot->regs.oct) + (result - 8);
}
if (result <= 0)    return 0;
if (result >= 0x3c) return 0x3c;
return result;
```

`KRS = 0xF` disables key-rate scaling. Otherwise the rate is doubled, offset by `2*KRS`, biased
by the top bit of `FNS`, and shifted by the (sign-mapped) octave. The result is clamped to
`[0, 0x3C]`.

**When does the envelope step?** `need_envelope_step()` (`scsp.c:649-690`):

| Effective rate | Behaviour |
|---|---|
| any, with `sample_counter == 0` | never |
| 0 or 1 | never |
| `>= 0x30` | step on every even `sample_counter` (once per 2 samples) |
| 2 … 0x2F | step when `sample_counter % envelope_table[rate-2][step_count] == 0`, then advance `step_count`, wrapping when the next entry is `EFFECTIVE_RATE_END` |

`envelope_table` (`scsp.c:241-268`) is 52 rows × 8 entries, built by `MAKE_TABLE(SHIFT)` for
`SHIFT = 0..12`. Each `MAKE_TABLE` emits four rows (rates ≡ 2,3,4,5 mod 4):

```c
#define MAKE_TABLE(SHIFT) \
 { 8192>>S, 4096>>S, 4096>>S, END, END, END, END, END }, \
 { 8192>>S, 4096>>S, 4096>>S, 4096>>S, 4096>>S, 4096>>S, 4096>>S, END }, \
 { 4096>>S, END, END, END, END, END, END, END }, \
 { 4096>>S, 4096>>S, 4096>>S, 2048>>S, 2048>>S, END, END, END },
```

with `EFFECTIVE_RATE_END == 0xffff` (`scsp.c:241`). The entries are *sample-counter moduli*, so
smaller values mean more frequent steps; each `SHIFT` increment halves the interval — one rate
octave per four rate values.

**Step size** — `attack_rate_table` (`scsp.c:195-216`) and `decay_rate_table` (`scsp.c:218-239`)
are 16 rows × 4 columns, indexed by `[effective_rate - 0x30]` (clamped to row 0 for rates
`<= 0x30`) and `[envelope_steps_taken & 3]`. Attack entries are *shift counts* 1-4; decay
entries are *linear increments* 1, 2, 4, 8.

**Phase logic** — `op4` (`scsp.c:733-771`):

| Phase | Action | Transition |
|---|---|---|
| `ATTACK` | `attenuation -= (attenuation >> attack_rate_table[...]) + 1` (exponential) | `attenuation == 0` → `DECAY1` |
| `DECAY1` | `do_decay(slot, D1R)`: `attenuation += decay_rate_table[...]`, capped at `< 0x3bf` | `(attenuation >> 5) >= DL` → `DECAY2` |
| `DECAY2` | `do_decay(slot, D2R)` | none — runs to silence |
| `RELEASE` | `do_decay(slot, RR)` | none |

`change_envelope_state()` (`scsp.c:643-647`) resets `step_count` on every transition.

**Final volume** — `apply_volume` (`scsp.c:773-786`), called from `op5`:

```c
v  = tl * 4;             // TL is 8 bits, scaled into the 10-bit attenuation domain
v += slot_att;           // = attenuation + amplitude-LFO contribution
if (v > 0x3ff) v = 0x3ff;
sample_att = (s * ((v & 0x3F) ^ 0x7F)) >> ((v >> 6) + 7);
```

— a 4-bit exponent (`v >> 6`, giving shifts 7..15) with a 6-bit inverted mantissa.

**[QUIRK]** `EGHOLD` is stored (`slot->regs.hold`) and forces `AR = 0x1F` at write time
(`scsp.c:1076`, `1296`, quoting *"SCSP Users manual 4.2"*) but has no other effect: there is no
hold-at-peak state.

**[QUIRK] Game-specific hack:** `if (slot->regs.ar < 0x010) slot->regs.ar = 0x10; // for Darius
Gaiden` (`scsp.c:1075` and `:1295`). Attack rates below 16 are silently clamped for *every*
game, permanently altering the register value (it reads back changed).

### 5.2 Old engine: fixed-point counter + rate tables + phase function pointers

Constants (`scsp.c:1604-1633`):

| Symbol | Value | Meaning |
|---|---|---|
| `SCSP_ENV_HB` / `SCSP_ENV_LB` | 10 / 10 | envelope counter integer / fractional bits |
| `SCSP_ENV_LEN` | 1024 | table length |
| `SCSP_ENV_AS` | 0 | attack start |
| `SCSP_ENV_DS` | `1024 << 10` = `0x100000` | decay start |
| `SCSP_ENV_AE` | `0x0FFFFF` | attack end |
| `SCSP_ENV_DE` | `0x1FFFFF` | decay end (= slot off) |
| `SCSP_ATTACK_R` | `8 * 44100` | attack base time |
| `SCSP_DECAY_R` | `12 * SCSP_ATTACK_R` | decay base time |

`scsp_env_table[2048]` (`scsp.c:4623-4634`) holds the attack curve in `[0..1023]` — `x^7`,
`env = 0x3FF - (((0x3FF-i)/1024)^7 * 1024)` — and a *linear* decay curve in `[1024..2047]`.
The table value is a **loudness multiplier** (higher = louder), the opposite polarity from the
new engine's attenuation.

`scsp_attack_rate[96]` / `scsp_decay_rate[96]` (`scsp.c:4686-4715`):

```c
for (i = 0; i < 60; i++) {
   x  = 1.0 + ((i & 3) * 0.25);                 // bits 0-1: x1.00, x1.25, x1.50, x1.75
   x *= (double)(1 << (i >> 2));                // bits 2-5: x2^0 .. x2^15
   x *= (double)(SCSP_ENV_LEN << SCSP_ENV_LB);
   scsp_attack_rate[i+4] = round(x / SCSP_ATTACK_R);   // min 1
   scsp_decay_rate[i+4]  = round(x / SCSP_DECAY_R);    // min 1
}
scsp_attack_rate[63] = SCSP_ENV_AE;   // AR=31 with no scaling = instant
scsp_decay_rate[61] = scsp_decay_rate[62] = scsp_decay_rate[63] = scsp_decay_rate[60];
for (i = 64; i < 96; i++) { attack[i] = attack[63]; decay[i] = decay[63]; null_rate[i-64] = 0; }
```

Rate lookup applies key-rate scaling and octave (`scsp_slot_refresh_einc`, `scsp.c:2134-2145`):

```c
slot->einca = slot->arp[(14 - slot->fsft) >> slot->krs];   // arp = &scsp_attack_rate[AR << 1]
```

with `krs` pre-converted (`KRS==0xF → 4`, else `KRS >>= 2`, `scsp.c:2255-2258`) and `fsft` the
octave shift (`scsp.c:2301-2304`: `OCT >= 8 ? 23-OCT : (OCT&7)^7`, giving `fsft ∈ [0,15]`).
`scsp2.c:3050` uses `(15 - octave_shift) >> krs_shift` — a **one-off difference** from
`scsp.c`'s `14 -`.

Phase advance is driven by function pointers (`scsp.c:2079-2129`) and the `SCSP_UPDATE_ENV`
macro (`scsp.c:3211-3217`):

| Phase | `einc` | `ecmp` (target) | `enxt` on reaching target |
|---|---|---|---|
| `ATTACK` | `&einca` | `SCSP_ENV_AE` | `scsp_attack_next`: `ecnt = DS`, `einc = &eincd`, `ecmp = sl`, phase `DECAY` |
| `DECAY` | `&eincd` | `slot->sl` | `scsp_decay_next`: `ecnt = sl`, `einc = &eincs`, `ecmp = DE`, phase `SUSTAIN` |
| `SUSTAIN` | `&eincs` | `SCSP_ENV_DE` | `scsp_sustain_next`: `ecnt = DE`, `einc = NULL`, `ecmp = DE+1`, `enxt = null` |
| `RELEASE` | `&eincr` | `SCSP_ENV_DE` | `scsp_release_next`: same terminal state |

`sl` (decay level) is stored pre-biased for the comparison:
`slot->sl = ((d & 0x3E0) << SCSP_ENV_LB) + SCSP_ENV_DS` (`scsp.c:2521`) — the comment says
"adjusted for envelope compare (ecmp)". The byte path builds it in two halves
(`scsp.c:2260-2262` for `DL[4:3]`, `scsp.c:2268-2270` for `DL[2:0]`); each half re-adds
`SCSP_ENV_DS`, but each also strips it first, because the retain-masks (`0xE0 << 10` = `0x38000`
and `0x300 << 10` = `0xC0000`) do not cover bit 20. The two paths therefore agree.

Note the ASCII art in the source (`scsp.c:2069-2077`):

```
   Max EG level = 0x3FF      /|\
                            / | \
                           /  |  \_____
   Min EG level = 0x000 __/   |  |    |\___
                          A   D1 D2   R
```

confirming the intended four-segment shape, and that in this engine `0x3FF` is the *loud* end.

### 5.3 `scsp2.c`: same maths, restructured

`scsp2.c` uses the same counter model (`SCSP_ENV_ATTACK_START/DECAY_START/ATTACK_END/DECAY_END`,
`scsp2.c:185-188`) but an `x^4` attack curve instead of `x^7` (`scsp2.c:822`), a straight
`0x3FF - i` decay curve (`scsp2.c:827`), and phase transitions inlined into the generated
audio functions (`scsp2.c:692-714`) rather than function pointers. Its TL table is
`round(pow(2.0, -(i/16.0)) * 1024)` (`scsp2.c:909`) versus `scsp.c`'s
`round(pow(10, (i * -0.3762)/20) * 1024)` (`scsp.c:4726`) — numerically the same −0.376 dB per
step, differently expressed.

---

## 6. LFO

### 6.1 New engine

Two 256-entry table sets, regenerated on every reset by `fill_plfo_tables()` (`scsp.c:414-452`)
and `fill_alfo_tables()` (`scsp.c:454-487`):

| Waveform | `PLFOWS`/`ALFOWS` | Pitch table (`s8`) | Amplitude table (`u8`) |
|---|---|---|---|
| Sawtooth | 0 | `i` for `i<128`, `-256+i` otherwise → −128…127 ramp | `i` → 0…255 ramp |
| Square | 1 | `+127` for `i<128`, `-128` otherwise | `0` for `i<128`, `0xFF` otherwise |
| Triangle | 2 | `i*2` (`i<64`), `255-i*2` (`i<192`), `i*2-512` (else) | `i*2` (`i<128`), `255-i*2` (else) |
| Noise | 3 | `rand() & 0xff` | `rand() & 0xff` |

**[QUIRK]** The "noise" waveforms are a *fixed* 256-entry random sequence generated once per
reset from the C library `rand()` — they repeat with the same period as the other waveforms
rather than being a free-running noise source. `scsp2.c:858-859` flags the same problem:
*"FIXME: note that the noise generator output should be independent of LFORE/LFOF"*.

Rate: `lfo_step_table[32]` (`scsp.c:355-389`) gives *samples per LFO table step*, from `0x3FC`
(LFOF 0, slowest) down to `0x001` (LFOF 31, fastest).

**[BUG]** The table is not monotonic. Grouped in fours the intended step is
−0x80, −0x40, −0x20, −0x10, …; the third group reads `0x0fc, 0x0bc, 0x0dc, 0x08c` where the
pattern demands `0x0fc, 0x0dc, 0x0bc, 0x09c` — entries 9 and 0xA are transposed and entry 0xB is
`0x08c` rather than `0x09c`. There is no comment explaining the deviation, so LFOF values 9-11
run at the wrong rate.

Application: pitch in `op1` (`scsp.c:519`), amplitude in `op5` (`scsp.c:811-812`):

```c
plfo_shifted = (plfo_val << slot->regs.plfos) >> 2;
lfo_add = (((alfo_val + 1)) >> (7 - slot->regs.alfos)) << 1;
sample  = apply_volume(slot->regs.tl, slot->state.attenuation + lfo_add, slot->state.output);
```

**[QUIRK]** As with `PLFOS`, `ALFOS = 0` yields a non-zero `lfo_add` (up to 4) rather than
disabling amplitude modulation.

**[QUIRK] `LFORE` is not implemented in the new engine.** The bit is decoded into `regs.re`
(`scsp.c:1114`, `1323`) and read back, but nothing ever tests it, so the LFO cannot be reset or
held.

### 6.2 Old engine

Tables are 1024 entries (`SCSP_LFO_LEN`, `scsp.c:1624`) with 10 fractional counter bits, built at
`scsp.c:4650-4684`:

| | Envelope (`_e`, unsigned) | Frequency (`_f`, signed, offset in the table) |
|---|---|---|
| Sawtooth | `0x3FF - i` | `scsp_lfo_sawt_f[(i+512) & 0x3FF] = i - 512` |
| Square | `0x3FF` for `i<512`, else 0 | `0x3FF-512-128` for `i<512`, else `-512+128` |
| Triangle | `0x3FF - i*2` / `(i-512)*2` | written at `[(i+768) & 0x3FF]` |
| Noise | `rand() & 0x3FF` | `noi_e[i] - 512` |

Rate table `scsp_lfo_step[32]` (`scsp.c:4636-4647`):

```c
for (i = 0, j = 0; i < 32; i++) {
   j += 1 << (i >> 2);                        // 1,2,3,4, 6,8,10,12, 16,...
   x = (SCSP_FREQ / 256.0) / (double)j;       // base LFO frequency ~172.3 Hz
   scsp_lfo_step[31 - i] = round(x * (SCSP_LFO_LEN / (double)SCSP_FREQ) * (1 << SCSP_LFO_LB));
}
```

Sensitivity encodings (`scsp.c:2350-2358`, `2567-2575`) — both are *shift counts*, and `31` is
the sentinel for "disabled":

```c
if ((d >> 5) & 7) slot->lfofms = ((d >> 5) & 7) + 7;  else slot->lfofms = 31;   // PLFOS
if (d & 7)        slot->lfoems = ((d & 7) ^ 7) + 4;   else slot->lfoems = 31;   // ALFOS
```

Application (`scsp.c:3131-3134`, `3194-3197`):

```c
slot->env = (scsp_env_table[ecnt >> ENV_LB] * slot->tl / 1024) - (slot->lfoemw[pos] >> slot->lfoems);
slot->fcnt += ((slot->lfofmw[pos] << (slot->lfofms - 7)) >> (slot->fsft + 1));
```

`LFORE = 1` sets `lfoinc = -1` and **returns early** (`scsp.c:2318-2322` byte path,
`scsp.c:2556-2560` word path); clearing it resets `lfocnt = 0` (`scsp.c:2323-2326`).

**[BUG]** the early return happens *before* any of the other fields in the same register are
decoded, so a single word write to `$12` with bit 15 set silently discards `LFOF`, `PLFOWS`,
`PLFOS`, `ALFOWS` and `ALFOS`. Since `LFORE` and the LFO parameters live in the same 16-bit
register, a driver that configures the LFO and asserts reset in one write loses the
configuration entirely.

**[QUIRK]** `lfoinc = -1` means the LFO counter *counts down by 1 per sample* rather than being
held — a sentinel value that also happens to be a valid increment. `scsp2.c:2708-2714` handles
`LFORE` correctly: it zeroes the counter, disables both modulation shifts, and still decodes
every other field first.

---

## 7. The sound DSP (`scspdsp.c` / `scspdsp.h`)

This is the SCSP's own 128-step effects DSP, wholly distinct from the SCU DSP. It exists only
in the `scsp.c` new-engine path (`scsp.c:1515-1516`). The state object is a single global
`ScspDsp scsp_dsp`, defined at `yabause/src/yabause.c:128` and declared `extern` in
`scspdsp.h:188`.

### 7.1 Register banks

| Bank | Type | Size | Register-block address | Purpose |
|---|---|---|---|---|
| `COEF` | `u16[64]` | 64 × 13-bit | `$700-$77F` | Multiplier Y-input coefficients |
| `MADRS` | `u16[32]` | 32 × 16-bit | `$780-$7BF` | Sound-RAM base addresses for `MRD`/`MWT` |
| `MPRO` | `u64[128]` | 128 × 64-bit | `$800-$BFF` | Microprogram |
| `TEMP` | `s32[128]` | 128 × 24-bit | `$C00-$DFF` nominal — **not mapped** | Ring-addressed scratch |
| `MEMS` | `s32[32]` | 32 × 24-bit | `$E00-$E7F` nominal — **not mapped** | Sound-RAM read results |
| `MIXS` | `s32[16]` | 16 × 20-bit | `$E80-$EBF` (read only) | Per-slot inputs, summed via `ISEL`/`IMXL` |
| `EFREG` | `s16[16]` | 16 × 16-bit | `$EC0-$EDF` | Effect outputs, returned to the mixer |
| `EXTS` | `s16[2]` | 2 × 16-bit | `$EE0`, `$EE2` | External (CD audio) inputs |

(`scspdsp.h:46-53`; address decode `scsp.c:4162-4234`, `4272-4318`, `4363-4370`, `4395-4402`,
`4444-4498`, `4534`.)

Storage details:

- **`COEF`** is stored **pre-shifted**: writes do `coef[n] = d >> 3` ("lower 3 bits seem to be
  discarded", `scsp.c:4275`), reads do `coef[n] << 3`. Byte writes reconstruct through the same
  `<<3` representation (`scsp.c:4165-4171`).
- **`MPRO`** is big-endian-ordered within each 8-byte slot: `$800` holds bits 63:48, `$802` bits
  47:32, `$804` bits 31:16, `$806` bits 15:0 (`scsp.c:4290-4305`). Byte writes cover all 8 offsets
  (`scsp.c:4189-4225`). Word writes set `scsp_dsp.updated = 1` (`scsp.c:4309`); **[BUG] byte
  writes do not**, so `last_step` (§7.4) is not recomputed after a byte-wise program upload.
- **`MIXS`** is 20-bit, read as two half-registers across a 4-byte stride
  (`scsp.c:4485-4491`): the even word returns `mixs[n] & 0x0000000F` and the odd word returns
  `(mixs[n] >> 4) & 0xFFFF`. That is *not* a high-nibble/low-word split. It is self-consistent
  with how `mixs` is actually filled — `mixs[isel] += mixs_input << 4` with a 16-bit
  `mixs_input` (`scsp.c:1494`), and consumed as `mixs[...] << 4` (`scspdsp.c:175`) — so the
  payload lives in bits 19:4 and bits 3:0 are always zero. Consequence: **the even word of every
  `MIXS` pair always reads back as 0.** No comment in the source explains the split, and nothing
  here establishes what real hardware returns.
- **`MADRS`** is 32 entries, but only 16 are readable: `$780-$79F` reads indices 0-15 and
  `$7A0-$7BF` is treated as a "madrs mirror" of the same 0-15 (`scsp.c:4449-4459`), while
  *writes* to `$7A0-$7BF` go to indices 16-31 (`scsp.c:4278-4282`, `4174-4183`). **[BUG]** the
  upper half of `MADRS` is write-only.
- **`TEMP` and `MEMS` have no address decode at all.** Writes fall into the `scsp_dcr` dead
  shadow, reads return 0 with a warning.

### 7.2 The MPRO instruction word

64 bits, laid out as documented in `scspdsp.h:90-106` and implemented by the bitfield union at
`scspdsp.h:146-182` (little-endian variant; a big-endian variant exists at `scspdsp.h:109-144`).

| Bits | Field | Width | Function (per `ScspDspExec`, `scspdsp.c:154-289`) |
|---|---|---|---|
| 63 | `unknown` | 1 | Not used; the disassembler prints "unknown" if set (`scspdsp.c:625-629`) |
| 62:56 | `TRA` | 7 | TEMP read address: `TEMP[(TRA + MDEC_CT) & 0x7F]` (`:168`, `:182`) |
| 55 | `TWT` | 1 | Enable TEMP write (`:211`) |
| 54:48 | `TWA` | 7 | TEMP write address: `TEMP[(TWA + MDEC_CT) & 0x7F]` (`:167`, `:212`) |
| 47 | `XSEL` | 1 | Multiplier X input: 0 = `TEMP`, 1 = `INPUTS` (`:183`, `:222`) |
| 46:45 | `YSEL` | 2 | Multiplier Y input: 0 = `FRC_REG`, 1 = `COEF[COEF]`, 2 = `(Y_REG >> 11) & 0x1FFF`, 3 = `(Y_REG >> 4) & 0x0FFF` (`:184-188`) |
| 44 | `unknown2` | 1 | Not used |
| 43:38 | `IRA` | 6 | INPUTS source select — see below (`:170-179`) |
| 37 | `IWT` | 1 | Enable `MEMS` write from the pending read result (`:237`) |
| 36:32 | `IWA` | 5 | `MEMS` write index (`:239`) |
| 31 | `TABLE` | 1 | 1 = absolute addressing (skip `MDEC_CT` add and ring-buffer wrap) (`:265-269`) |
| 30 | `MWT` | 1 | Schedule a sound-RAM write (`:277`) |
| 29 | `MRD` | 1 | Schedule a sound-RAM read (`:273`) |
| 28 | `EWT` | 1 | Enable `EFREG` write (`:208`) |
| 27:24 | `EWA` | 4 | `EFREG` write index; value written is `ShifterOutput >> 8` (`:209`) |
| 23 | `ADRL` | 1 | Load `ADRS_REG` (`:282`) |
| 22 | `FRCL` | 1 | Load `FRC_REG` (`:214`) |
| 21 | `SHIFT1` | 1 | High bit of the 2-bit SHIFT field (`:197`, `:199`) |
| 20 | `SHIFT0` | 1 | Low bit of SHIFT (`:197`, `:218`, `:286`) |
| 19 | `YRL` | 1 | Load `Y_REG` from `INPUTS & 0xFFFFFF` (`:191-193`) |
| 18 | `NEGB` | 1 | Negate the adder's B input (`:228`) |
| 17 | `ZERO` | 1 | Force the adder's B input to 0 (overrides `NEGB`) (`:231`) |
| 16 | `BSEL` | 1 | Adder B input: 0 = `TEMP`, 1 = `SHIFT_REG` (accumulate) (`:189`, `:226`) |
| 15 | `NOFL` | 1 | 1 = no float conversion on `MRD`/`MWT` (`:275`, `:280`) |
| 14:9 | `COEF` | 6 | `COEF[]` index for `YSEL == 1` (`:185`) |
| 8:7 | `unknown3` | 2 | Not used |
| 6:2 | `MASA` | 5 | `MADRS[]` index for the address generator (`:257`) |
| 1 | `ADREB` | 1 | Add sign-extended `ADRS_REG` to the address (`:260-263`) |
| 0 | `NXADR` | 1 | Add 1 to the address (`:258`) |

**SHIFT decoding is not a plain 2-bit field.** The shift *amount* is `SHIFT0 ^ SHIFT1` and
saturation is applied only when `SHIFT1 == 0` (`scspdsp.c:197-205`):

| `SHIFT` (bits 21:20) | Left shift | Saturate to 24-bit signed? |
|---|---|---|
| 0 (`00`) | 0 | yes |
| 1 (`01`) | 1 | yes |
| 2 (`10`) | 1 | no |
| 3 (`11`) | 0 | no |

`SHIFT == 3` (`SHIFT0 & SHIFT1`) additionally selects the *alternate* source for `FRC_REG`
(`ShifterOutput & 0xFFF` instead of `ShifterOutput >> 11`, `scspdsp.c:216-218`) and for
`ADRS_REG` (`ShifterOutput >> 12` instead of `(INPUTS >> 16) & 0xFFF`, `scspdsp.c:284-286`).

**`IRA` decoding** (`scspdsp.c:170-179`):

| `IRA` | Source |
|---|---|
| `0x00-0x1F` | `MEMS[IRA & 0x1F]` (24-bit, used as-is) |
| `0x20-0x2F` | `MIXS[IRA & 0xF] << 4` |
| `0x30-0x31` | `EXTS[IRA & 1] << 8` |
| `0x32-0x3F` | **nothing** — `dsp->inputs` keeps its previous value (`if (!(ira & 0xE))` gate) |

### 7.3 Execution semantics

`ScspDspExec(dsp, addr, sound_ram)` (`scspdsp.c:154-289`) executes exactly one instruction. The
ordering within a step is:

1. Compute `TEMPWriteAddr`/`TEMPReadAddr` from `TWA`/`TRA` + `MDEC_CT`, wrapped to 7 bits.
2. Latch `INPUTS` from `IRA`.
3. Sign-extend `INPUTS` and `TEMP` to 24 bits; build the X/Y/B selector arrays.
4. If `YRL`, latch `Y_REG = INPUTS & 0xFFFFFF`.
5. **Shifter** operates on the *previous* step's `SHIFT_REG`:
   `ShifterOutput = sign26(SHIFT_REG) << (SHIFT0^SHIFT1)`, optional saturation, `& 0xFFFFFF`.
6. If `EWT`, `EFREG[EWA] = ShifterOutput >> 8`. If `TWT`, `TEMP[TWA'] = ShifterOutput`.
   If `FRCL`, latch `FRC_REG`.
7. **Multiplier:** `product = (sign13(Y) * X) >> 12` — Y is treated as 13-bit signed regardless
   of which `YSEL` source it came from (`scspdsp.c:222`).
8. **Adder:** `SHIFT_REG = (product + B) & 0x3FFFFFF` with `B` from `BSEL`, negated by `NEGB`,
   zeroed by `ZERO` (`scspdsp.c:224-234`).
9. If `IWT`, `MEMS[IWA] = read_value` (the result of a read scheduled *two* steps earlier).
10. Service one pending sound-RAM read *or* write (never both in the same step,
    `scspdsp.c:242-253`).
11. **Address generator:** `addr = MADRS[MASA] + NXADR (+ sign12(ADRS_REG) if ADREB)`; unless
    `TABLE`, `addr += MDEC_CT` and `addr &= (0x2000 << RBL) - 1`; then
    `io_addr = (addr + (RBP << 12)) & 0x3FFFF` — **word** units into sound RAM.
12. Schedule the next read (`read_pending = 1 + NOFL`) or write
    (`write_value = NOFL ? ShifterOutput >> 8 : int_to_float(ShifterOutput)`).
13. If `ADRL`, latch `ADRS_REG`.

**Float format.** `float_to_int` (`scspdsp.c:84-106`) decodes a 16-bit value as
sign(15) : exponent(14:11) : mantissa(10:0), with exponents above 11 clamped and the implicit
leading bit synthesised as `sign` vs `!sign`; `int_to_float` (`scspdsp.c:108-151`) is the
inverse, normalising a 24-bit value by repeated ×2/×8/×64 with exponent accumulation. `NOFL = 1`
bypasses both, using a raw `<< 8` / `>> 8`.

### 7.4 Driving the DSP

From `generate_sample()` (`scsp.c:1498-1524`), once per output sample:

```c
scsp_dsp.rbp = rbp;  scsp_dsp.rbl = rbl;
scsp_dsp.exts[0] = cd_in_l;  scsp_dsp.exts[1] = cd_in_r;

if (scsp_dsp.updated) {                      // recompute program length
   for (i = 127; i >= 0; --i) if (scsp_dsp.mpro[i] != 0) break;
   scsp_dsp.last_step = i + 1;
   scsp_dsp.updated = 0;
}
for (i = 0; i < scsp_dsp.last_step; i++) ScspDspExec(&scsp_dsp, i, SoundRam);

if (!scsp_dsp.mdec_ct) scsp_dsp.mdec_ct = (0x2000 << rbl);
scsp_dsp.mdec_ct--;

for (i = 0; i < 16; i++) scsp_dsp.mixs[i] = 0;   // MIXS is cleared every sample
```

**[QUIRK] Program length is inferred from trailing zeros.** An all-zero instruction word is
treated as the end of the program, so a legitimate `nop` in the middle is fine but a program
ending in `nop`s runs short — and one whose last instruction happens to encode as 0 is truncated.
`ScspDspAssembleLine` explicitly maps the mnemonic `nop` to `instruction.all = 0`
(`scspdsp.c:431-434`).

**[QUIRK] `MDEC_CT` decrements once per output sample**, i.e. the ring-buffer pointer advances
one word per 44.1 kHz sample, and wraps at `0x2000 << RBL`.

### 7.5 Assembler / disassembler

`ScspDspAssembleLine` (`scspdsp.c:300-437`), `ScspDspAssembleFromFile` (`:439-457`),
`ScspDspDisasm` (`:459-642`) and `ScspDspDisassembleToFile` (`:644-662`) are debug tools, not
hardware. Two defects worth knowing if they are used to cross-check a Mimas implementation:

- **[BUG]** `if (strstr(line, "nxadr")) instruction.part.adreb = 1;` (`scspdsp.c:426-429`) — the
  `nxadr` mnemonic sets `ADREB`, not `NXADR`.
- **[BUG]** `instruction.part.shift1 = ScspDspAssembleGetValue(temp);` (`scspdsp.c:381-384`) —
  a 2-bit `shift N` operand is written into the 1-bit `shift1` field, so `shift 1` and `shift 3`
  assemble identically to `shift 1`/`shift 1`. The disassembler mirrors the mistake
  (`scspdsp.c:547-551`).

### 7.6 Dead code in `scspdsp.c` / `ScspDsp`

`saturate_24()` (`scspdsp.c:44-53`), `clz()` (`scspdsp.c:59-81`) and the `min` macro
(`scspdsp.c:57`) are defined and never called. The `ScspDsp` fields `b`, `x`, `y`, `acc`,
`shifted`, `mul_out`, `mrd_value`, `need_read`, `need_nofl`, `need_write`, `write_data`
(`scspdsp.h:57-77`) are never read or written by the DSP — they survive only in the savestate
(`scsp.c:6317-6332`). Conversely `product`, `read_value`, `write_value`, `read_pending`,
`write_pending` and `shift_reg` — the fields the DSP *does* use — are **not** saved.

---

## 8. Sound-RAM DMA

### 8.1 `scsp.c` — `scsp_dma()` (`scsp.c:1955-1996`)

Triggered synchronously from the `$416` write handler, in both the byte and word paths:

```c
case 0x16: scsp.dmlen = (scsp.dmlen & 0xFE) + ((d & 0xF) << 8);
           if ((scsp.dmfl = d & 0xF0) & 0x10) scsp_dma();     // :2774-2777
case 0x16: scsp.dmlen = d & 0xFFE;
           if ((scsp.dmfl = ((d >> 8) & 0xF0)) & 0x10) scsp_dma();   // :2931-2934
```

`dmfl` holds `DGATE` as `0x40`, `DDIR` as `0x20`, `DEXE` as `0x10`. The transfer is
**instantaneous** — the whole thing runs inside the register write, regardless of length. Word
count is `dmlen >> 1`.

```c
if (scsp.dmfl & 0x20) {                 // DDIR == 1  -- comment says "dsp -> scsp_ram"
   u32 from = scsp.dmea;  u32 to = scsp.drga;
   for (i = 0; i < cnt; i++) { u16 val = scsp_r_w(from);
                               T2WriteWord(SoundRam, to & 0x7FFFF, val); from += 2; to += 2; }
} else {                                // DDIR == 0  -- "scsp_ram -> dsp"
   u32 from = scsp.dmea;  u32 to = scsp.drga;
   for (i = 0; i < cnt; i++) { u16 val = T2ReadWord(SoundRam, from & 0x7FFFF);
                               scsp_w_w(to, val); from += 2; to += 2; }
}
scsp.dmfl &= ~0x10;                     // clear DEXE
scsp_ccr[0x16 ^ 3] &= 0xE0;             // clear DEXE and DTLG[11:8] in the shadow
scsp_sound_interrupt (0x10);
scsp_main_interrupt (0x10);
```

**[BUG] `DDIR == 1` uses `DMEA` as the *register* address and `DRGA` as the *sound-RAM* address**
— exactly backwards from the register names, and backwards from the same function's own
`DDIR == 0` branch. `scsp.c`'s own header comment (`scsp.c:69-71`) defines
`DMEAL/DMEAH = "transfer start address (sound)"` and `DRGA = "start register address (dsp)"`,
so `DMEA` should be the sound-RAM side in *both* directions.

**[QUIRK] `DDIR` polarity differs between the two implementations.** `scsp.c` treats
`DDIR == 0` as sound-RAM → registers; `scsp2.c` treats `DDIR == 1` as RAM → registers
(`scsp2.c:3156`, comment `// {RAM,zero} -> registers`). These files do not agree and nothing in
the source set resolves which is correct.

**[QUIRK] `DGATE` is not implemented in `scsp.c`.** The zero-fill lines are commented out
(`scsp.c:1968`, `1985`: `//if (scsp.dmfl & 0x40) val = 0;`).

**[QUIRK] `DMEA` is only 19 bits in `scsp.c`** — the `$414` handler masks with `0x70`/`0x7000`
(`scsp.c:2766`, `2927`), dropping `DMEA[19]`. This covers the full 512 KB of sound RAM, so it is
benign, but it differs from `scsp2.c:2861`, which keeps all four bits.

**[QUIRK] No arbitration, no bus stealing, no timing.** The transfer neither costs cycles nor
blocks the M68K or the SH-2.

### 8.2 `scsp2.c` — `ScspDoDMA()` (`scsp2.c:3152-3191`)

The same instantaneous model, but consistent about addresses and with `DGATE` implemented:

| `DDIR` | `DGATE` | Action |
|---|---|---|
| 1 | 0 | `ScspWriteWordDirect(DRGA+i, T2ReadWord(SoundRam, DMEA+i))` |
| 1 | 1 | `ScspWriteWordDirect(DRGA+i, 0)` |
| 0 | 0 | `T2WriteWord(SoundRam, DMEA+i, ScspReadWordDirect(DRGA+i))` |
| 0 | 1 | `memset(&SoundRam[DMEA], 0, DTLG)` |

Loop step is `i += 2` up to `DTLG`, `DMEA` pre-masked by `sound_ram_mask`. It also calls
`M68K->WriteNotify(dmea, dtlg)` for the RAM-destination cases (`scsp2.c:3185`), which `scsp.c`
does **not** — **[BUG]** in `scsp.c`, a DMA into sound RAM does not invalidate any M68K
recompiler cache. On completion: `dexe = 0`, clear bit 12 of the `$416` cache, raise interrupt
4 to both CPUs.

---

## 9. Timers and interrupts

### 9.1 Timers A/B/C

`scsp_update_timer(u32 len)` (`scsp.c:3966-4003`) advances all three counters by `len` output
samples:

```c
scsp.timacnt += len << (8 - scsp.timasd);
if (scsp.timacnt >= 0xFF00) { scsp_sound_interrupt(0x40); scsp_main_interrupt(0x40);
                              scsp.timacnt -= 0xFF00; }
// ... same for B (0x80) and C (0x100) ...
if (len) { scsp_sound_interrupt(0x400); scsp_main_interrupt(0x400); }
```

Each counter is 8.8 fixed point: writing `TIMA` sets `timacnt = d << 8` (`scsp.c:2788`), and the
per-sample increment is `1 << (8 - TxCTL)`, so `TxCTL = 0` ticks once per sample and `TxCTL = 7`
once per 128 samples. Overflow at `0xFF00` (i.e. the counter reaching 0xFF) fires the interrupt
and subtracts, not resets. `scsp2.c:1406-1416` is identical; it additionally uses
`ScspTimerCyclesLeft()` (`scsp2.c:1396-1399`) to chop the execution slice so that an enabled
timer interrupt is delivered on time even when the SCSP thread runs a large batch of cycles.

**[QUIRK] Interrupt 10 (`0x400`, "once per output sample") fires once per *call*, not once per
sample.** With the new engine and both async paths, `scsp_update_timer(1)` is called once per
generated sample, so it comes out right (`scsp.c:5332`, `5609`, `5692`). In the synchronous
non-`ASYNC_SCSP` build it is instead called from `ScspExec` with `len = scsptiming2 >> 16`
(`scsp.c:5540-5543`) — the whole number of samples elapsed since the last call — and the
interrupt still fires only once, so every sample in that batch after the first loses its
interrupt. The code acknowledges it: `// 1F interrupt can't be accurate here...`
(`scsp.c:3997`). Note the same collapsing applies to timers A/B/C, which can only fire once per
`scsp_update_timer` call regardless of how many `0xFF00` crossings `len` implies
(`scsp.c:3976`: `timacnt -= 0xFF00`, subtract-once; `scsp2.c:1413` says so outright:
`// We won't pass 0xFF00 multiple times at once`).

### 9.2 Interrupt sources

The bit assignments are only *named* in `scsp2.c:194-202`; `scsp.c` uses bare masks. They agree:

| Bit | Mask | Name | Raised by |
|---|---|---|---|
| 3 | `0x008` | MIDI input data available | `scsp.c:4013`, `4053`; `scsp2.c:3122` |
| 4 | `0x010` | DMA complete | `scsp.c:1994-1995`; `scsp2.c:3190` |
| 5 | `0x020` | Manual — write 1 to bit 5 of `SCIPD`/`MCIPD` | `scsp.c:2822`, `2859`, `2958`, `2991`; `scsp2.c:2904`, `2935` |
| 6 | `0x040` | Timer A | `scsp.c:3974-3975` |
| 7 | `0x080` | Timer B | `scsp.c:3983-3984` |
| 8 | `0x100` | Timer C | `scsp.c:3992-3993` |
| 9 | `0x200` | MIDI output buffer became empty | `scsp.c:4099-4100` |
| 10 | `0x400` | Once per output sample | `scsp.c:4000-4001` |

Bits 0-2 are never raised by anything in the source set.

### 9.3 Sound-CPU (M68K) path

```c
void scsp_sound_interrupt (u32 id) {      // :1937-1948
  scsp.scipd |= id;
  if (scsp.scieb & id) scsp_trigger_sound_interrupt (id);
}
static INLINE void scsp_trigger_sound_interrupt (u32 id) {    // :1879-1922
   u32 level = 0;
   if (id > 0x80) id = 0x80;              // bits 8..10 all share SCILVn bit 7
   if (scsp.scilv0 & id) level |= 1;
   if (scsp.scilv1 & id) level |= 2;
   if (scsp.scilv2 & id) level |= 4;
   scsp.sintf (level);                    // -> c68k_interrupt_handler -> M68K->SetIRQ(level)
}
```

`sintf` is `c68k_interrupt_handler` (`scsp.c:4850-4855`), installed by `ScspInit`
(`scsp.c:5086`). `SCILV0/1/2` are three parallel 8-bit registers forming a 3-bit IPL *per
interrupt source*; sources 8-10 are folded onto bit 7 because the registers are only 8 bits wide.

**[BUG]** `scsp_trigger_sound_interrupt` calls `scsp.sintf(level)` unconditionally, so a source
whose `SCILVn` bits are all clear asserts IPL 0 on the M68K — spuriously de-asserting whatever
higher-priority request was outstanding. The alternative routine `scsp_check_interrupt()`
(`scsp.c:1847-1877`), used on every `SCIEB`/`SCIRE`/`SCILVn` write, does it correctly: it scans
all enabled+pending sources, takes the maximum level, and only calls `sintf` when `level != 0`.
Note that the two routines are used at different times, so which behaviour applies depends on
whether the change came from a register write or from an interrupt source firing. The
"correct" body is even present, `#if 0`'d out, inside `scsp_trigger_sound_interrupt`
(`scsp.c:1888-1914`) — **dead code**.

`scsp2.c:3214-3242` uses a single path (`ScspRaiseInterrupt`) with
`level_shift = (which > 7) ? 7 : which` and calls `M68K->SetIRQ(level)` only when the source is
enabled — but likewise without a maximum-priority scan.

### 9.4 Main-CPU (SH-2 / SCU) path — the origin of "Sound Request"

```c
void scsp_main_interrupt (u32 id) {              // :1925-1935
  scsp.mcipd |= id;
  if (scsp.mcieb & id) scsp_trigger_main_interrupt (id);
}
static INLINE void scsp_trigger_main_interrupt (u32 id) {   // :1840-1845
  scsp.mintf ();
}
static void scu_interrupt_handler (void) {       // :4859-4864
  ScuSendSoundRequest ();
}
```

`mintf` is `scu_interrupt_handler`, installed by `ScspInit` at `scsp.c:5086`. So:

> **The SCSP asserts the SCU's "Sound Request" line whenever any bit is set in `MCIPD`
> (`$42C`) that is also set in `MCIEB` (`$42A`).** There is no vector, no level and no
> per-source distinction on this path — the SH-2 side sees a single edge and must read
> `MCIPD` to find out why.

The main-CPU path carries *no* level information at all: `MCIEB`/`MCIPD` use the same 11 bit
positions as `SCIEB`/`SCIPD`, but there is no `MCILVn`.

Writing `MCIEB` as a word additionally re-scans for already-pending interrupts
(`scsp.c:2978-2988`):

```c
case 0x2A: scsp.mcieb = d;
   for (i = 0; i < 11; i++)
      if (scsp.mcieb & (1 << i) && scsp.mcipd & (1 << i)) scsp_trigger_main_interrupt((1 << i));
```

**[BUG]** the *byte* writes to `$42A`/`$42B` (`scsp.c:2850-2856`) do **not** do this scan, so a
byte-wise unmask of an already-pending main-CPU interrupt is lost. (The corresponding sound-CPU
byte writes at `$41E`/`$41F` do call `scsp_check_interrupt()`.) `scsp2.c:2514-2523` handles both
byte halves via `ScspCheckInterrupts`.

**[BUG]** the high-byte writes to `SCIEB`/`MCIEB` do not mask the incoming data to 3 bits:
`scsp.scieb = (scsp.scieb & 0xFF) + (d << 8)` (`scsp.c:2810`, `2851`) lets bits 11-15 into a
register that is architecturally 11 bits. The low-byte writes *do* mask
(`(scsp.scieb & 0x700) + d`, `scsp.c:2817`). `scsp2.c:2496`, `2515` masks with `0x07`.

**[QUIRK]** in `scsp2.c`, `ScspCheckInterrupts` (`scsp2.c:3251-3262`) tests
`(1<<i) & mask & scsp.mcieb && scsp.mcipd` — the pending register is tested for *any* bit set,
not the matching bit. That is almost certainly meant to be `scsp.mcipd & (1<<i)`.

**[QUIRK]** In the threaded `scsp2.c` build, main-CPU interrupts are not delivered from the SCSP
thread; they set `scsp_main_interrupt_pending` and are dispatched by the main thread on its next
`ScspExec` (`scsp2.c:1226-1230`, `3222-3225`).

### 9.5 MIDI

`scsp.c:4008-4127` implements four-byte in/out FIFOs with the flag byte `scsp.midflag`
(`SCSP_MIDI_IN_EMP 0x01`, `IN_FUL 0x02`, `IN_OVF 0x04`, `OUT_EMP 0x08`, `OUT_FUL 0x10`,
`scsp.c:1598-1602`). `scsp_midi_in_send` raises interrupt 3 when the buffer transitions from
empty; `scsp_midi_out_read` raises interrupt 9 when the out buffer empties. Reads return `0xFF`
on an empty FIFO.

The Saturn has no MIDI port. The whole subsystem is only reachable when the optional
`USE_SCSPMIDI` build flag is on (`CMakeLists.txt:209-212`), which adds `MidiIn`/`MidiOut`
callbacks to `SoundInterface_struct` (`scsp.h:74-78`) and pumps them once per frame
(`scsp.c:5848-5866`). `scsp2.c:3104-3107` says so directly: *"Since there is no facility for
sending MIDI data (the Saturn does not have a MIDI I/O port), most of this is essentially a
giant no-op, but the logic is included for reference."* — and `scsp2.c` has no `MidiIn`/`MidiOut`
hooks at all, so its FIFOs can only ever be driven by `$406` writes.

---

## 10. M68000-side memory map

### 10.1 Bus handlers

Installed by `ScspInit` (`scsp.c:5074-5082`):

```c
M68K->SetReadB ((C68K_READ  *)c68k_byte_read);
M68K->SetReadW ((C68K_READ  *)c68k_word_read);
M68K->SetWriteB((C68K_WRITE *)c68k_byte_write);
M68K->SetWriteW((C68K_WRITE *)c68k_word_write);
M68K->SetFetch (0x000000, 0x040000, (pointer)SoundRam);
M68K->SetFetch (0x040000, 0x080000, (pointer)SoundRam);
M68K->SetFetch (0x080000, 0x0C0000, (pointer)SoundRam);
M68K->SetFetch (0x0C0000, 0x100000, (pointer)SoundRam);
```

The handlers (`scsp.c:4781-4846`) are all of the form:

```c
static u32 FASTCALL c68k_byte_read (const u32 adr) {
  u32 rtn = 0;
  if (adr < 0x100000) { if (adr < 0x80000) rtn = T2ReadByte(SoundRam, adr & 0x7FFFF); }
  else                  rtn = scsp_r_b(adr);
  return rtn;
}
```

giving this map:

| M68K address | Contents | Notes |
|---|---|---|
| `0x000000-0x07FFFF` | Sound RAM, 512 KB, direct | Masked `& 0x7FFFF` for bytes (`scsp.c:4787`, `4805`); **not** masked for words/longs (`scsp.c:4822`, `4840`) — harmless, since the range check already bounds it |
| `0x080000-0x0FFFFF` | **Nothing.** Reads return 0, writes are discarded | `scsp.c:4785-4789`, `4800-4807` |
| `0x100000-0xFFFFFF` | SCSP register block, mirrored every `0x1000` | `scsp_r_b/w` mask `a &= 0xFFF`/`0xFFE` internally |

Note the **data path ignores `MEM4MB` entirely** — the M68K always sees the full 512 KB and never
the 256 KB mirroring, while the *fetch* path is reconfigured by `MEM4MB` (§2.2) and the SH-2
data path honours it (§10.2). **[QUIRK]**, a three-way inconsistency.

`scsp2.c:3528-3557` is cleaner and consistent: it masks with `scsp.sound_ram_mask` for everything
below `0x100000` and routes `>= 0x100000` to `Scsp{Read,Write}{Byte,Word}Direct(address & 0xFFF)`.
It also has no dead `0x080000-0x0FFFFF` hole — sound RAM mirrors across the whole first megabyte.

The M68K has only byte and word accessors; there is no long path
(`m68kcore.h:63-66`, `scsp.h:167`). Long accesses are synthesised by the 68000 core itself
(`c68k.c:205-211`).

### 10.2 SH-2-side sound RAM window

`SoundRamRead/Write{Byte,Word,Long}` (`scsp.c:4868-5056`) apply, in order:

1. `addr &= 0xFFFFF` — the window is 1 MB.
2. If `MEM4MB == 0`, `addr &= 0x3FFFF` — mirror every 256 KB.
3. Else if `addr > 0x7FFFF`, return all-ones (reads) / discard (writes).

Writes additionally call `M68K->WriteNotify(addr, size)` (`scsp.c:4900`, `4979`, `5053`).

**[BUG]** `SoundRamReadByte` (`scsp.c:4868-4883`) sets `val = 0xFF` for out-of-range addresses
and then **immediately overwrites it** with `T2ReadByte(SoundRam, addr)`, reading up to 512 KB
past the end of the 0x80000-byte allocation:

```c
u8 FASTCALL SoundRamReadByte (u32 addr) {
  addr &= 0xFFFFF;
  u8 val = 0;
  if (scsp.mem4b == 0) addr &= 0x3FFFF;
  else if (addr > 0x7FFFF) val = 0xFF;     // <-- dead store
  val = T2ReadByte(SoundRam, addr);        // <-- out-of-bounds when mem4b && addr > 0x7FFFF
  return val;
}
```

`SoundRamReadWord` and `SoundRamReadLong` handle the same case correctly with an early `return`.

### 10.3 SH-2 ↔ M68K synchronisation

`SyncSh2And68k()` (`scsp.c:4908-4942`) is called from `SoundRamReadWord` and
`SoundRamReadLong` (but **not** from `SoundRamReadByte`, nor from any write). Every 512th sound
RAM read it signals the SCSP thread:

```c
if (mem_access_counter++ >= 512) {
   if (g_scsp_main_mode == 0) {            // only the CPU-time-driven thread waits on this
      pthread_mutex_lock(&sync_mutex); pthread_cond_signal(&sync_cnd); pthread_mutex_unlock(&sync_mutex);
   }
   mem_access_counter = 0;
}
```

with the constant justified in a comment as `// Memory Access cycle = 128 times per 44.1Khz /
28437500 / 4410 / 128 = 50` (`scsp.c:4919-4920`). **[QUIRK]** this is a heuristic, not a
hardware behaviour; the SH-2 is neither stalled nor charged cycles for sound-RAM contention.

---

## 11. Execution and timing model

The SCSP runs on its own thread in the default `ASYNC_SCSP` configuration
(`scsp.h:97-99`: `#if !defined(YAB_SYNC_SCSP) #define ASYNC_SCSP`). Two thread bodies exist,
chosen by `g_scsp_main_mode` (`scsp.c:5771-5782`):

| Mode | Function | Pacing |
|---|---|---|
| 0 | `ScspAsynMainCpuTime` (`scsp.c:5554-5635`) | Follows the SH-2's M68K cycle counter (`getM68KCounter() >> SCSP_FRACTIONAL_BITS`, `SCSP_FRACTIONAL_BITS == 20`, `yabause.h:154`), sleeping 250 µs when it has nothing to do |
| 1 (default) | `ScspAsynMainRealtime` (`scsp.c:5638-5768`) | Free-runs against the wall clock, `nanosleep`ing to `16666666 / frame_div` ns per frame |

Both run the M68K in `samplecnt = 256` cycle chunks — one 44.1 kHz sample at the SCSP's
11.2896 MHz clock (`scsp.c:5559`: `// 11289600/44100`) — and call
`new_scsp_exec(samplecnt << 1)` (new engine) or `scsp_update_timer(1)` (old engine).
`new_scsp_exec_real` in turn generates one sample per 512 units (`scsp.c:5349-5358`), which is
why the cycle count is doubled at the call site.

Sample buffering: the new engine writes into `new_scsp_outbuf_l/r[900]` (`scsp.c:160-161`) and
`new_scsp_update_samples` copies out at most `scspsoundlen` samples per frame
(`scsp.c:5510-5523`). **[QUIRK]** overrun past 900 samples is silently discarded
(`scsp.c:5340-5343`, `// buffer overrun`) and any samples generated beyond `scspsoundlen` in a
frame are dropped, not carried over.

**[QUIRK] Game-specific hack:** `ScspAsynMainRealtime` special-cases Thunder Force V
(`scsp.c:5662-5668`):

```c
char * pCurrentGame = Cs2GetCurrentGmaecode();
if (!strcmp(pCurrentGame, "T-1811G") && frame_div < 4) {
   frame_div = 4;
   framecnt = 188160 / frame_div;
   LOG("Thunder Force V is detected. Force frame_div to 4");
}
```

quadrupling the SCSP↔video sync rate for that one disc ID.

**Dead code:** `scsp.c:5005-5031` contains an `#if 0` block that, for game codes `T-1229G` and
`T-1228G`, would busy-wait in `SoundRamReadLong` at address `0x500` until the M68K wrote a
non-`0xFFFFFFFF`, non-zero value. It is disabled and labelled `// This is the workround`.

In the synchronous (`!ASYNC_SCSP`) build, both `M68KExec` (`scsp.c:5297-5309`) and
`new_scsp_exec` (`scsp.c:5367-5379`) batch ten deciline calls into one
(`AUDIO_BATCH_LIMIT 10`, `scsp.c:5289-5290`) — **[QUIRK]**, an explicit performance/accuracy
trade-off, documented in the comments as *"cutting the number of indirect calls into the M68K
core by AUDIO_BATCH_LIMIT-x without touching the caller's cadence"*.

`scsp2.c` has its own threading design (`scsp2.c:45-74`): the thread loops on `ScspDoExec()`
against `scsp_clock_target`, capped at `SCSP_CLOCK_MAX_EXEC = SCSP_CLOCK_FREQ / 1000` cycles per
iteration, and external register writes are marshalled through a three-variable shared buffer
(`scsp_write_buffer_{size,address,data}`, `scsp2.c:511-516`) that the writer spins on until the
SCSP thread consumes it (`scsp2.c:1590-1643`).

---

## 12. CDDA input

Two entirely different paths.

**Old engine** (`scsp.c:1803-1809`, `3897-3940`, `5477-5507`): a 150-sector (`2*75`) ring buffer;
`ScspReceiveCDDA` copies whole 2352-byte sectors in and nudges the CD block's timing
(`Cs2SetTiming`) depending on fill level; `scsp_update` then adds the 16-bit LE stereo pairs
straight into the output buffers *after* all slot mixing, bypassing the SCSP's level, pan and
master-volume stages entirely.

**New engine** (`scsp.c:5312-5333`): consumes 4 bytes (one stereo frame) per generated sample and
feeds them to `EXTS0`/`EXTS1`, i.e. into the DSP's external inputs and then through the
`EFSDL`/`EFPAN` of slots 16 and 17 (§4.4) — much closer to the hardware routing.

**`scsp2.c`** (`scsp2.c:1475-1502`, `1651-1668`) uses a 3-sector buffer plus a
`CDDA_DELAY_SAMPLES = 100` startup delay *"to avoid audio popping when the SCSP emulation gets a
few samples ahead of the CDDA input"* (`scsp2.c:554-556`), and mixes post-slot like the old
engine.

---

## 13. Savestates

`SoundSaveState`/`SoundLoadState` (`scsp.c:6060-6692`) write tag `"SCSP"` version 3. Relevant to
Mimas only as evidence of what the authors considered *architectural* state:

- M68K registers (or `M68K->SaveState` under `IMPROVED_SAVESTATES`), `IsM68KRunning`, `savedcycles`.
- `use_new_scsp`, `new_scsp_cycles`, the 64-entry sound stack, and every field of all 32
  `new_scsp.slots[]` — registers *and* internal state.
- The whole `scsp_reg[0x1000]` raw shadow, and all 512 KB of sound RAM.
- Old-engine `slot_t` internals, with `einc` and `enxt` function pointers serialised as small
  integer tags (`scsp.c:6194-6224`, restored at `6500-6546`) and `buf8`/`buf16` regenerated from
  `sa`/`pcm8b`/`lea` on load (`scsp.c:6592-6609`).
- All `scsp_t` globals, the 64-entry `scsp.stack`, and most of `ScspDsp`.

On load, version > 1 replays every slot register through `scsp_w_w` to regenerate derived state
(`scsp.c:6462-6470`) — a useful hint that all slot-derived caches must be reconstructible from
the raw register words alone.

**[BUG]** `scsp_dsp.product`, `read_value`, `write_value`, `read_pending`, `write_pending` and
`shift_reg` are not saved (§7.6), so a state loaded mid-program resumes the DSP with a stale
accumulator and a lost pending memory operation.

---

## 14. Known deviations / gaps in this implementation

Everything below is a place where the Yabause source does something other than model the
hardware faithfully. Section references point back into this document.

### 14.1 Not implemented at all

| # | Item | Where |
|---|---|---|
| 1 | `SSCTL` sound sources 1 (internal noise) and 2 (internal zeros) — slots are silenced instead, with only the phase counter advanced | §4.5; `scsp.c:3855-3876`, `scsp2.c:3091-3093` |
| 2 | `SBCTL` source bit reversal — decoded, never applied by any engine | §3.1 |
| 3 | `STWINH` (stack write inhibit) — the new engine's `op7` always writes the sound stack | §3.1, §4 |
| 4 | `SDIR` (sound direct) — decoded, never applied | §3.1 |
| 5 | `LPSLNK` (start D1R on reaching LSA) — decoded, never applied by any engine | §3.1 |
| 6 | `EGHOLD` — only forces `AR = 0x1F`; there is no hold-at-peak envelope state | §5.1 |
| 7 | `LFORE` (LFO reset) — not implemented in the new engine at all; the old engine turns it into a −1 LFO increment | §6 |
| 8 | `DAC18B` — not decoded by `scsp.c`; stored but inert in `scsp2.c` | §2.1 |
| 9 | Sample interpolation — `op4` is captioned "interpolation, eg" but only runs the envelope; every engine point-samples | §4.3 |
| 10 | Modulation (`MDL`/`MDXSL`/`MDYSL`) in the old engine and in `scsp2.c` | §0.1, §4.2 |
| 11 | The entire sound DSP in the old engine and in `scsp2.c`; both substitute "play the effect send dry" | §0.2, §4.5 |
| 12 | Master volume (`MVOL`) in the old engine and in `scsp2.c` | §4.5 |
| 13 | `DGATE` (DMA zero-fill) in `scsp.c` — the lines are commented out | §8.1 |
| 14 | DSP `TEMP` (`$C00-$DFF`) and `MEMS` (`$E00-$E7F`) register windows — no address decode; writes land in a dead shadow, reads return 0 | §1.2, §7.1 |
| 15 | Any DMA/bus timing, arbitration or contention: DMA is instantaneous, sound-RAM access costs nobody any cycles | §8.1, §10.3 |
| 16 | `MCILVn` — there is no per-source level for the main-CPU interrupt path, and no such register exists | §9.4 |

### 14.2 Wrong or self-contradictory

| # | Item | Where |
|---|---|---|
| 17 | `scsp_dma()` `DDIR==1` swaps the roles of `DMEA` and `DRGA`, contradicting the file's own register-map comment | §8.1 |
| 18 | `DDIR` polarity is *opposite* between `scsp.c` and `scsp2.c`; nothing in the source resolves which is right | §8.1 |
| 19 | `lfo_step_table[]` entries 9, 0xA, 0xB break the table's own geometric progression (0x0bc/0x0dc transposed, 0x08c should be 0x09c) | §6.1 |
| 20 | Old engine stores `STWINH` into the variable named `sdir` and `SDIR` into `swe` | §3.1 |
| 21 | Old engine's `$12` write returns early when `LFORE` is set, so `LFOF`, `PLFOWS`, `PLFOS`, `ALFOWS` and `ALFOS` in the *same* write are silently discarded | §6.2; `scsp.c:2318-2322`, `2556-2560` |
| 22 | `PLFOS == 0` and `ALFOS == 0` do not disable pitch/amplitude modulation in the new engine | §4.1, §6.1 |
| 23 | New-engine panning uses the 4-bit pan magnitude as a shift → 6 dB/step instead of ~3 dB/step | §4.4 |
| 24 | `AddSoundPan` prints the pan direction inverted relative to every decoder | §4.4 |
| 25 | `$408` byte and word reads disagree about the `CA` bit position (`0xE0` vs `0x780`), and the new engine never shifts `ca` into place | §2.4 |
| 26 | `SoundRamReadByte` has a dead `val = 0xFF` store and reads up to 512 KB out of bounds when `MEM4MB` is set | §10.2 |
| 27 | `scsp_trigger_sound_interrupt` asserts M68K IPL 0 for sources with no `SCILVn` bits set, clobbering a pending higher-priority request | §9.3 |
| 28 | Byte writes to `MCIEB` (`$42A`/`$42B`) skip the already-pending rescan that the word write performs | §9.4 |
| 29 | High-byte writes to `SCIEB`/`MCIEB` do not mask the value to 11 bits | §9.4 |
| 30 | `scsp2.c`'s `ScspCheckInterrupts` tests `scsp.mcipd`/`scsp.scipd` for *any* set bit rather than the matching bit | §9.4 |
| 31 | DSP `MADRS[16..31]` are writable but not readable (`$7A0-$7BF` reads mirror indices 0-15) | §7.1 |
| 32 | DSP `MIXS` word pairs split as `bits 3:0` / `bits 19:4`, so the even word of every pair always reads back as 0; unexplained and unverifiable from this source | §7.1 |
| 33 | Byte writes to `MPRO` do not set `scsp_dsp.updated`, so `last_step` is not recomputed | §7.1 |
| 34 | 32-bit reads and writes of `COEF`, `MADRS` and `MPRO` do nothing (writes go to the dead shadow, reads return 0) | §1.2 |
| 35 | Byte reads of `COEF`, `MADRS` and `MPRO` return 0, although byte *writes* work | §1.2 |
| 36 | Word/byte writes to `EFREG` and `MIXS` go to the dead shadow; only a long write reaches `EFREG`, and it truncates to 16 bits and misses the second word | §1.2, §7.1 |
| 37 | The `scsp_w_d` `EFREG` branch has no `return`, so every long write to `$EC0-$EDF` also logs "unhandled" | §1.2 |
| 38 | `scsp_w_b`'s DSP-shadow range test is `a > 0xC00` rather than `>= 0xC00`, so `$C00` exactly is unhandled | §1.2 |
| 39 | `MEM4MB` is honoured by the SH-2 data path and the M68K *fetch* path, but ignored by the M68K *data* path | §10.1 |
| 40 | With `MEM4MB` set, M68K fetch banks above `0x090000` keep stale pointers from the previous configuration | §2.2 |
| 41 | `scsp.c`'s DMA does not call `M68K->WriteNotify` after writing sound RAM | §8.2 |
| 42 | `scsp_dsp` savestate omits `product`, `read_value`, `write_value`, `read_pending`, `write_pending`, `shift_reg` while saving eleven fields the DSP never touches | §13, §7.6 |
| 43 | DSP assembler: `nxadr` sets `ADREB`; `shift N` writes a 2-bit operand into the 1-bit `shift1` field | §7.5 |
| 44 | `scspdsp.h`'s big-endian instruction union declares `u32 all` where the little-endian one has `u64 all` — the top 32 bits are unreachable on big-endian builds | `scspdsp.h:143` vs `:181` |
| 45 | `scsp2.c` comments `efpan` as `[2:0]`; the code correctly decodes `[4:0]` | `scsp2.c:291` vs `:2750` |

### 14.3 Deliberate shortcuts and approximations

| # | Item | Where |
|---|---|---|
| 46 | Loop wrap in mode 1 is a hard `sample_offset = LSA` reset, discarding phase overshoot (new engine and old engine; `scsp2.c` does it with a modulo) | §4.2 |
| 47 | Reverse and alternating loops (`LPCTL` 2 and 3) are unimplemented in the old engine and in `scsp2.c` (`// FIXME: reverse/alternating loops not implemented`, `scsp2.c:676`) | §4.2 |
| 48 | New-engine key-off does not convert an in-progress attack into a release; old engine and `scsp2.c` do | §3.3 |
| 49 | New-engine key-on starts the attack from attenuation `0x280`, not from silence or from the current value; the old engine's own comment calls its equivalent "probably wrong" | §3.3, §5.2 |
| 50 | `op3` always masks the sample address with `0x7FFFF` regardless of `MEM4MB`, bypassing `SoundRamReadWord` | §4.3 |
| 51 | `MDXSL`/`MDYSL` are masked to 5 bits, so the current generation of the sound stack (entries 32-63) is unreachable | §4.2 |
| 52 | DSP program length is inferred from the last non-zero `MPRO` word | §7.4 |
| 53 | LFO "noise" waveforms are a fixed 256- (or 1024-) entry `rand()` sequence regenerated only on reset | §6.1 |
| 54 | Interrupt 10 ("once per sample") fires once per `scsp_update_timer` *call*, which in the synchronous build is once per deciline | §9.1 |
| 55 | `SyncSh2And68k` signals the SCSP thread every 512th sound-RAM read as a heuristic stand-in for real bus contention | §10.3 |
| 56 | Synchronous builds batch ten decilines of M68K and SCSP execution into one call (`AUDIO_BATCH_LIMIT`) | §11 |
| 57 | New-engine output buffer is a fixed 900 samples; overruns are silently dropped | §11 |
| 58 | CDDA is mixed *after* the SCSP output stage in the old engine and in `scsp2.c`, bypassing level/pan/MVOL | §12 |
| 59 | The old engine and `scsp2.c` reroute the effect send to the direct output when the direct send is muted, as a stand-in for the missing DSP | §4.5 |
| 60 | MIDI is fully modelled but unreachable without the optional `USE_SCSPMIDI` build flag, and the Saturn has no MIDI port | §9.5 |
| 61 | 32 specialised audio-generation functions (old engine) / 25 macro-generated ones (`scsp2.c`) exist purely as a speed optimisation; the "null" variant is used for every silent case | §4.5, `scsp2.c:736-799` |
| 62 | `scsp2.c`'s frequency-modulation path carries `// FIXME: need to handle the case where LFO data range != 1<<FREQ_LOW_BITS` | `scsp2.c:668` |
| 63 | `scsp2.c` key-on carries `// FIXME: should this start at the current value if the old sound is still decaying?` | `scsp2.c:2987` |
| 64 | `scsp2.c` carries `// FIXME: If a bit is already 1 in both SCIEB and SCIPD, does writing another 1 here (no change) trigger another interrupt or not?` — the semantics of an enable-register write are genuinely unresolved in this source | `scsp2.c:2896-2898` |

### 14.4 Game-specific hacks

| # | Item | Where |
|---|---|---|
| 65 | `if (slot->regs.ar < 0x010) slot->regs.ar = 0x10; // for Darius Gaiden` — applied to every game, and it mutates the stored register value | `scsp.c:1075`, `:1295` |
| 66 | Thunder Force V (`T-1811G`) forces `frame_div = 4` in the realtime SCSP thread | `scsp.c:5662-5668` |
| 67 | The dummy M68K core zeroes ten hard-coded sound-RAM words (`0x700`-`0x770`, `0x790`, `0x792`) on every "execution" so SH-2 sound-driver handshakes do not deadlock | `m68kcore.c:63-76` |

### 14.5 Dead code

| # | Item | Where |
|---|---|---|
| 68 | `op6` ("level calc 2") is an empty function occupying a pipeline stage | `scsp.c:818-821` |
| 69 | The correct maximum-priority interrupt scan inside `scsp_trigger_sound_interrupt` is `#if 0`'d out | `scsp.c:1888-1914` |
| 70 | The `T-1229G`/`T-1228G` busy-wait workaround in `SoundRamReadLong` is `#if 0`'d out | `scsp.c:5005-5031` |
| 71 | `saturate_24()`, `clz()` and the `min` macro in `scspdsp.c` are defined and never called | `scspdsp.c:44-81` |
| 72 | `ScspDsp` fields `b`, `x`, `y`, `acc`, `shifted`, `mul_out`, `mrd_value`, `need_read`, `need_nofl`, `need_write`, `write_data` are never used by the DSP | `scspdsp.h:57-77` |
| 73 | The DSP's sound-RAM write guard `if (!(dsp->io_addr & 0x40000))` can never be false — `io_addr` is already masked to `0x3FFFF` | `scspdsp.c:250`, `:271` |
| 74 | `SlotRegs.kx` (KYONEX) is declared and serialised but never assigned | `scsp.c:273`, `:6100` |
| 75 | `scsp.stack[64]` in the old engine's `scsp_t` is declared and serialised but never written | `scsp.c:1764`, `:6305` |
| 76 | The debug-instrument mute system (`NUM_DEBUG_INSTRUMENTS 24`, `scsp_debug_*`) is a developer tool that hooks the mixer, active only when `new_scsp.debug_mode` is set | `scsp.c:834-925`, `:1473-1477` |
| 77 | Unused local `int i;` declarations in the `SCIEB` byte-write cases | `scsp.c:2809`, `:2816` |
