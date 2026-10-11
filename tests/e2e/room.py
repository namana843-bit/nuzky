"""Reduce echo, Match loudness and Even out volume on a talking head in a ringing room whose speaker turns away for two
phrases: turned on in the inspector's Voice section by keyboard and pointer, prepared in the background, and heard in
the export as a shorter ring after the phrases, the clip at -16 LUFS and the quiet phrases closer to the loud ones, at
the same length and with every sound on its sample."""
import json, re, subprocess

import numpy as np

from e2e.harness import FIXTURES, export, flow, press, wait

SOURCE = FIXTURES / 'room.mp4'
RATE = 48_000
WINDOW = RATE // 50  # 20 ms
JOB = "return window.__nuzky.store.getState().jobs[arguments[0]] ?? null"
CONTENT = "return window.__nuzky.store.getState().snap.project.tracks[0].clips[0].content"
CHECKBOX = ("[...document.querySelectorAll('aside[aria-label=Inspector] label')]"
            ".find((l) => l.textContent === arguments[0])?.querySelector('input')")


def decode(path):
    raw = subprocess.run(['ffmpeg', '-v', 'error', '-i', str(path), '-vn', '-ac', '1', '-ar', str(RATE), '-f', 'f32le', '-'],
                         capture_output=True, check=True).stdout
    return np.frombuffer(raw, dtype='<f4').astype(np.float64)


def levels(samples):
    """Level of each 20 ms window in dBFS."""
    n = len(samples) // WINDOW
    return 10 * np.log10(np.mean(samples[:n * WINDOW].reshape(n, WINDOW) ** 2, axis=1) + 1e-12)


def phrases(db):
    """The six phrases of the fixture as (first, last + 1) windows: louder than 30 dB under the loudest window, apart by
    more than 0.6 s."""
    loud = np.flatnonzero(db > db.max() - 30)
    groups, start = [], loud[0]
    for a, b in zip(loud, loud[1:]):
        if b - a > 30:
            groups.append((start, a + 1))
            start = b
    return groups + [(start, loud[-1] + 1)]


def ring(samples, spans):
    """Energy 60-300 ms after each phrase against the phrase's, in dB."""
    energy = lambda a, b: float(np.sum(samples[a * WINDOW:b * WINDOW] ** 2))
    tail = sum(energy(b + 3, b + 15) for _, b in spans)
    return 10 * np.log10(tail / sum(energy(a, b) for a, b in spans))


def integrated(path):
    log = subprocess.run(['ffmpeg', '-nostdin', '-hide_banner', '-i', str(path), '-vn', '-af', 'ebur128', '-f', 'null', '-'],
                         capture_output=True, text=True).stderr
    summary = log[log.rfind('Summary:'):].split()
    return float(summary[summary.index('I:') + 1])


@flow('room', 'Reduce echo, Match loudness and Even out volume, turned on in the Voice section, shorten the ring of a room, put the clip at -16 LUFS and bring a speaker who turned away closer in the export, every sound on its sample')
def room(r):
    if not SOURCE.exists():
        raise RuntimeError('tmp-test/room.mp4 is missing: run scripts/fixtures.sh')
    r.import_media(SOURCE)
    r.add_clip('room.mp4')
    clip = r.track()[0]
    off = export(r, 'off.mp4')

    r.focus_clip(clip['id'])
    tab = "document.querySelector('aside[aria-label=Inspector] [role=tab][aria-selected=true]')"
    if not wait(lambda: r.s.run(f'return !!{tab}'), 10):
        raise RuntimeError('the inspector shows no tabs for the clip')
    r.s.run(f'{tab}.focus()')
    r.key('End')
    r.check('End in the tab bar opens the Audio tab', wait(lambda: r.s.run(f'return {tab}?.textContent') == 'Audio', 5),
            r.s.run(f'return {tab}?.textContent'))
    content = lambda: r.s.run(CONTENT)
    # Keyboard: Tab from Clean voice reaches Reduce echo, and Space turns it on at 80 %.
    r.s.run(f'{CHECKBOX}.focus()', 'Clean voice')
    press('Tab')
    focus = "const e = document.activeElement; return e.type === 'checkbox' ? e.closest('label').textContent : e.outerHTML.slice(0, 120)"
    focused = wait(lambda: r.s.run(focus) == 'Reduce echo', 3, 0.05) or r.s.run(focus)
    r.check('Tab from Clean voice reaches the Reduce echo checkbox', focused is True, focused)
    press('space')
    r.check('Space turns Reduce echo on at 80 %', wait(lambda: content().get('dereverb') == 0.8, 5), content())
    r.check('Reduce by shows under it', wait(lambda: r.s.run("return document.querySelector('input[aria-label=\"Reduce by value\"]')?.value"), 5),
            r.s.run("return document.querySelector('input[aria-label=\"Reduce by value\"]')?.value ?? null"))
    r.s.run(f'{CHECKBOX}.click()', 'Match loudness')
    r.check('a click turns Match loudness on', wait(lambda: content().get('normalize') is True, 5), content())
    r.s.run(f'{CHECKBOX}.focus()', 'Even out volume')
    press('space')
    r.check('Space turns Even out volume on at 50 %', wait(lambda: content().get('leveler') == 0.5, 5), content())

    asset = clip['assetId']
    for kind in ('dereverb', 'level'):
        job = wait(lambda: (j := r.s.run(JOB, f'{kind}:{asset}')) and j['status'] != 'running' and j, 120)
        r.check(f'the {kind} preparation runs in the background and finishes', job and job['status'] == 'done', job)
    r.check('the status row goes once everything is ready',
            wait(lambda: not r.s.run("return !!document.querySelector('aside[aria-label=Inspector] [role=status]')"), 10))
    gain = ("return [...document.querySelectorAll('aside[aria-label=Inspector] label')].find((l) => l.textContent === 'Match loudness')"
            "?.parentElement.querySelector('.tabular')?.textContent ?? null")
    shown = wait(lambda: (g := r.s.run(gain)) and g, 10) or r.s.run(gain)
    r.check('Match loudness shows the gain it gives, in dB', bool(shown) and re.fullmatch(r'[+−]\d+\.\d dB', shown), shown)
    r.shot('inspector-voice')
    saved = lambda: {k: json.loads(r.saved_project().read_text())['tracks'][0]['clips'][0]['content'].get(k)
                     for k in ('dereverb', 'normalize', 'leveler')}
    r.check('the project file on disk has the three settings', wait(lambda: saved() == {'dereverb': 0.8, 'normalize': True, 'leveler': 0.5}, 10), saved())
    on = export(r, 'on.mp4')

    plain, voiced = decode(off), decode(on)
    r.check('the export is as long as before, to the sample', len(plain) == len(voiced), [len(plain), len(voiced)])
    spans = phrases(levels(plain))
    r.check('the fixture has six phrases', len(spans) == 6, spans)
    # Every phrase lines up with the original: the strongest correlation is at lag 0.
    lags = []
    for a, b in spans:
        x, y = plain[a * WINDOW:b * WINDOW], voiced[a * WINDOW - 480:b * WINDOW + 480]
        corr = np.correlate(y, x, mode='valid')
        lags.append(int(np.argmax(corr)) - 480)
    r.check('every phrase is on the sample it was', lags == [0] * len(spans), lags)
    before, after = ring(plain, spans), ring(voiced, spans)
    r.check('the ring after the phrases drops by 6 dB or more', after <= before - 6, f'{before:.1f} dB to {after:.1f} dB')
    lufs = integrated(on)
    r.check('the clip plays at -16 LUFS', abs(lufs + 16) <= 1, f'{lufs:.1f} LUFS, before {integrated(off):.1f}')
    level = lambda samples, picked: np.mean([np.mean(np.sort(levels(samples)[a:b])[(b - a) // 2:]) for a, b in picked])
    loud, quiet = [spans[i] for i in (0, 1, 4, 5)], spans[2:4]
    gap_before, gap_after = level(plain, loud) - level(plain, quiet), level(voiced, loud) - level(voiced, quiet)
    r.check('the phrases said away from the phone come at least 3 dB closer to the others', gap_after <= gap_before - 3,
            f'{gap_before:.1f} dB apart, then {gap_after:.1f} dB')

    r.key('z', ctrlKey=True)
    r.check('Ctrl+Z turns Even out volume off again in one step', wait(lambda: not content().get('leveler'), 5), content())
    r.check('no error toast', not r.errors(), r.errors())
