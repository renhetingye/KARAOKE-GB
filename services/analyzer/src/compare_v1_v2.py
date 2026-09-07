import json

def compare_charts():
    with open("tests/fixtures/shining_star_full_draft_v1_raw.json", "r", encoding="utf-8") as f:
        v1 = json.load(f)
    with open("tests/fixtures/shining_star_full_draft.json", "r", encoding="utf-8") as f:
        v2 = json.load(f)

    v1_notes = v1["notes"]
    v2_notes = v2["notes"]

    def stats(notes):
        durs = [(n["endUs"] - n["startUs"]) / 1000.0 for n in notes]
        micro_80 = [d for d in durs if d <= 80.0]
        short_100 = [d for d in durs if d <= 100.0]
        return {
            "count": len(notes),
            "mean_dur_ms": sum(durs) / len(durs) if durs else 0,
            "micro_80ms_count": len(micro_80),
            "short_100ms_count": len(short_100),
            "micro_80ms_ratio": len(micro_80) / len(durs) if durs else 0,
        }

    s1 = stats(v1_notes)
    s2 = stats(v2_notes)

    print("=== Comparison: v1 (Raw 1.4-semitone) vs v2 (Refined + Vibrato Filter + Onset) ===")
    print(f"Total Notes: v1={s1['count']} -> v2={s2['count']}")
    print(f"Mean Note Duration: v1={s1['mean_dur_ms']:.1f}ms -> v2={s2['mean_dur_ms']:.1f}ms")
    print(f"Micro Notes (<= 80ms): v1={s1['micro_80ms_count']} ({s1['micro_80ms_ratio']*100:.1f}%) -> v2={s2['micro_80ms_count']} ({s2['micro_80ms_ratio']*100:.1f}%)")
    print(f"Short Notes (<= 100ms): v1={s1['short_100ms_count']} -> v2={s2['short_100ms_count']}")

    print("\n--- Vocal Intro Section (34.0s - 36.0s) Comparison ---")
    print(">> v1 Notes:")
    for n in v1_notes:
        s_ms = n["startUs"] / 1000.0
        e_ms = n["endUs"] / 1000.0
        if 34000 <= s_ms <= 36000:
            print(f"   [{s_ms:.0f}ms - {e_ms:.0f}ms] dur={(e_ms-s_ms):.0f}ms, midi={n['pitchMidi']} (id={n['id']})")

    print(">> v2 Notes:")
    for n in v2_notes:
        s_ms = n["startUs"] / 1000.0
        e_ms = n["endUs"] / 1000.0
        if 34000 <= s_ms <= 36000:
            print(f"   [{s_ms:.0f}ms - {e_ms:.0f}ms] dur={(e_ms-s_ms):.0f}ms, midi={n['pitchMidi']} (id={n['id']})")

if __name__ == "__main__":
    compare_charts()
