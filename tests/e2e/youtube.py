"""YouTube music stays behind the rights notice, previews and adds without credit, and ducks in the export.
All yt-dlp lookups and downloads are served by the executable fake, without contacting YouTube."""
import json, statistics, subprocess, time
from pathlib import Path

from e2e.ducking import CHECKBOX, SOURCE, TONE, WINDOW, RATE, decode, speech_levels, tone_levels
from e2e.harness import flow, export, press, wait
from e2e.layout import resize

SONG = 'ytSong00001'
ROW = f'[data-sound="youtube:{SONG}"]'
FIELD = 'input[aria-label="Paste a YouTube link"]'
DIALOG = 'dialog[aria-labelledby="youtube-rights-title"][open]'
CLICK = 'document.querySelector(arguments[0]).click()'
PROJECT = 'return window.__nuzky.store.getState().snap.project'
PREVIEW = 'return window.__nuzky.sounds.getState().preview'


def prepare(r):
    audio = r.work / 'tone.m4a'
    subprocess.run(['ffmpeg', '-v', 'error', '-f', 'lavfi', '-i',
                    f'sine=frequency={TONE}:sample_rate={RATE}:duration=20', '-c:a', 'aac', str(audio)], check=True)
    r.log = r.work / 'ytdlp.jsonl'
    r.log.touch()
    r.env.update(NUZKY_YTDLP=str(Path(__file__).with_name('fake_ytdlp')),
                 NUZKY_FAKE_YTDLP_AUDIO=str(audio), NUZKY_FAKE_YTDLP_LOG=str(r.log))


def missing(r):
    r.env['NUZKY_YTDLP'] = str(r.work / 'not-installed')


def music(r):
    r.s.run("window.__nuzky.store.setState({panelTab: 'audio'})")
    wait(lambda: r.s.run("return !!document.querySelector('#audio-tab-music')"), 5)
    r.s.run(CLICK, '#audio-tab-music')
    r.check('Music shows Paste a YouTube link', wait(lambda: r.s.run('return !!document.querySelector(arguments[0])', FIELD), 5))


def paste(r, url):
    if r.s.run('return !!document.querySelector("button[aria-label=\\"Clear link\\"]")'):
        r.s.run(CLICK, 'button[aria-label="Clear link"]')
    r.s.run("""const input = document.querySelector(arguments[0]); input.focus();
const event = new Event('paste', {bubbles: true, cancelable: true});
Object.defineProperty(event, 'clipboardData', {value: {getData: () => arguments[1]}});
input.dispatchEvent(event);""", FIELD, url)


def dialog(r):
    return r.s.run('return !!document.querySelector(arguments[0])', DIALOG)


def answer(r, text):
    r.s.run("[...document.querySelectorAll(arguments[0] + ' button')].find((b) => b.textContent === arguments[1]).click()", DIALOG, text)


def invocations(r):
    return [json.loads(line) for line in r.log.read_text().splitlines()]


def alert(r):
    return r.s.run('return document.querySelector("[role=alert]")?.innerText ?? ""')


@flow('youtube', __doc__.split('.')[0], before=prepare)
def youtube(r):
    r.import_media(SOURCE)
    r.add_clip(SOURCE.name)
    music(r)
    paste(r, f'https://music.youtube.com/watch?v={SONG}&si=test')
    r.check('pasting first opens the rights notice with Cancel and I understand',
            wait(lambda: dialog(r), 5) and r.s.run("return document.querySelector(arguments[0]).innerText.includes('Music from YouTube is yours to clear') && [...document.querySelectorAll(arguments[0] + ' button')].map((b) => b.textContent).join('|') === 'Cancel|I understand'", DIALOG))
    r.check('yt-dlp has not run before the notice is answered', invocations(r) == [], invocations(r))
    time.sleep(0.4)
    r.shot('rights-dialog')
    answer(r, 'Cancel')
    r.check('Cancel closes the notice without looking up or adding anything',
            wait(lambda: not dialog(r), 5) and invocations(r) == [] and len(r.state()['assets']) == 1)
    paste(r, f'https://youtu.be/{SONG}?si=test')
    r.check('pasting after Cancel asks again and still runs nothing', wait(lambda: dialog(r), 5) and invocations(r) == [])
    answer(r, 'I understand')
    row = wait(lambda: r.s.run('return document.querySelector(arguments[0])?.innerText ?? ""', ROW), 10) or ''
    r.check('the song row has its title, artist, YouTube and duration',
            all(text in row for text in ('Steady Song', 'Test Artist · YouTube', '20.0 s')), row)
    r.check('YouTube has no licence chip', r.s.run('return !!document.querySelector(arguments[0]) && !document.querySelector(arguments[0] + " button[aria-label^=CC]")', ROW))
    settings = r.work / 'data/nuzky/sound-library.json'
    r.check('accepting saves youtubeRights on disk', settings.exists() and json.loads(settings.read_text()).get('youtubeRights') is True)
    r.check('looking up runs only metadata and adds nothing',
            len(invocations(r)) == 1 and '--skip-download' in invocations(r)[0] and len(r.state()['assets']) == 1, invocations(r))
    r.shot('looked-up-row')

    r.s.run(CLICK, ROW + ' button[aria-label^=Play]')
    playing = wait(lambda: (p := r.s.run(PREVIEW)) and p['id'] == f'youtube:{SONG}' and not p['loading'], 15)
    r.check('Play downloads and previews the song with a Stop button',
            playing and r.s.run('return !!document.querySelector(arguments[0])', ROW + ' button[aria-label="Stop Steady Song"]')
            and any('-o' in args for args in invocations(r)), invocations(r))
    r.check('previewing adds nothing', len(r.state()['assets']) == 1)
    r.s.run(CLICK, ROW + ' button[aria-label="Stop Steady Song"]')
    r.check('Stop ends the preview', wait(lambda: r.s.run(PREVIEW) is None, 5))
    r.seek(1_000_000)
    wait(lambda: r.s.run('return window.__nuzky.store.getState().timeUs') == 1_000_000, 5)
    r.s.run(CLICK, ROW + ' button[aria-label^=Add]')
    wait(lambda: len(r.state()['assets']) == 2, 15)
    project = r.s.run(PROJECT)
    asset = next((a for a in project['assets'] if a['name'] == 'Steady Song'), None)
    r.check('+ adds the song as an audio asset without credit', asset and asset['kind'] == 'audio' and asset.get('credit') is None, asset)
    if not asset:
        raise RuntimeError('the YouTube song was not added')
    placed = [(t, c) for t in project['tracks'] for c in t['clips'] if c['content'].get('assetId') == asset['id']]
    r.check('the music clip starts at the playhead on an audio track',
            len(placed) == 1 and placed[0][0]['kind'] == 'audio' and placed[0][0].get('keepInPlace') is True
            and placed[0][1]['startUs'] == 1_000_000, placed)
    file = r.work / f'data/nuzky/sounds/youtube-{SONG}.m4a'
    r.check('the added file is kept in the library with the downloaded audio',
            Path(asset['path']).resolve() == file.resolve() and file.read_bytes() == (r.work / 'tone.m4a').read_bytes())
    r.check('adding reuses the preview download', sum('-o' in args for args in invocations(r)) == 1, invocations(r))
    r.shot('clip-on-timeline')
    off = export(r, 'without-ducking.mp4')
    r.focus_clip(placed[0][1]['id'])
    r.check('the inspector offers Lower under speech, initially off',
            wait(lambda: r.s.run(f'return {CHECKBOX}?.checked === false'), 5))
    r.s.run(f'{CHECKBOX}.click()')
    duck = 'return window.__nuzky.store.getState().snap.project.tracks.flatMap((t) => t.clips).find((c) => c.id === arguments[0])?.content.duckDb'
    r.check('Lower under speech turns on for the YouTube clip', wait(lambda: r.s.run(duck, placed[0][1]['id']) == 12, 5))
    on = export(r, 'lower-under-speech.mp4')
    filters = f'highpass=f={TONE - 500},highpass=f={TONE - 500}'
    gain = [b - a for a, b in zip(tone_levels(decode(off, filters)), tone_levels(decode(on, filters)))]
    voice = speech_levels(decode(SOURCE, 'anull'))
    loudest = max(voice)
    speech = [i for i, v in enumerate(voice) if v > loudest - 15 and i >= RATE // WINDOW]
    runs, start = [], None
    for i, quiet in enumerate([v < loudest - 35 for v in voice] + [False]):
        if quiet and start is None:
            start = i
        elif not quiet and start is not None:
            runs.append((start, i))
            start = None
    first, last = max(runs, key=lambda run: run[1] - run[0])
    pause = list(range(max(first + 14, RATE // WINDOW), last - 4))
    under, back = statistics.median(gain[i] for i in speech), statistics.median(gain[i] for i in pause)
    r.check('the exported music is 12 dB lower under speech', abs(under + 12) < 1.5, f'{under:+.1f} dB')
    r.check('the exported music returns to full level in the long pause', abs(back) < 1, f'{back:+.1f} dB')

    before = r.s.run(PROJECT)
    calls = len(invocations(r))
    paste(r, 'https://youtube.com/playlist?list=PLtest')
    error = wait(lambda: alert(r), 5) or ''
    r.check('a playlist shows a plain error under the field', 'playlist' in error and 'one song' in error and 'PLAYLIST:' not in error, error)
    r.check('a playlist adds nothing and never runs yt-dlp', r.s.run(PROJECT) == before and len(invocations(r)) == calls)
    r.shot('playlist-error')
    paste(r, 'https://youtu.be/ytPrivate01')
    error = wait(lambda: alert(r), 10) or ''
    r.check('a private video shows a plain error and adds nothing',
            'private' in error and 'SOUND_UNAVAILABLE:' not in error and r.s.run(PROJECT) == before, error)
    paste(r, f'https://youtu.be/{SONG}')
    r.check('another paste shows the row without asking about rights again',
            wait(lambda: r.s.run('return !!document.querySelector(arguments[0])', ROW), 10) and not dialog(r))
    r.s.run(CLICK, 'button[aria-label="Clear link"]')
    r.s.run("""const input = document.querySelector(arguments[0]); input.focus();
Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, arguments[1]);
input.dispatchEvent(new Event('input', {bubbles: true}));""", FIELD, f'https://youtu.be/{SONG}')
    press('Return')
    r.check('Enter also looks up a typed link without another rights notice',
            wait(lambda: r.s.run('return !!document.querySelector(arguments[0])', ROW), 10) and not dialog(r))

    slow = '[data-sound="youtube:ytSlow00001"]'
    paste(r, 'https://youtu.be/ytSlow00001')
    wait(lambda: r.s.run('return !!document.querySelector(arguments[0])', slow), 10)
    assets = len(r.state()['assets'])
    r.s.run(CLICK, slow + ' button[aria-label^=Add]')
    wait(lambda: any('-o' in args and args[-1].endswith('ytSlow00001') for args in invocations(r)), 10)
    started = time.time()
    # The button stops only on a single click, as a double click on + must not stop the add it started.
    r.s.run("document.querySelector(arguments[0]).dispatchEvent(new MouseEvent('click', {bubbles: true, detail: 1}))",
            slow + ' button[aria-label="Stop adding Slow Song"]')
    stopped = wait(lambda: r.s.run('return window.__nuzky.sounds.getState().adding.length === 0'), 5)
    took = time.time() - started
    time.sleep(0.5)
    left = [p.name for p in r.work.rglob('*') if p.name.startswith('.nuzky-part-') or 'ytSlow' in p.name]
    r.check('Stop ends a YouTube download within seconds, adds nothing and leaves no part of it behind',
            stopped and took < 3 and len(r.state()['assets']) == assets and left == [], (round(took, 1), left))
    r.check('every invocation gets only a canonical watch URL',
            bool(invocations(r)) and all(args[args.index('--') + 1:] in ([f'https://www.youtube.com/watch?v={v}'] for v in (SONG, 'ytPrivate01', 'ytSlow00001'))
                                         for args in invocations(r)), invocations(r))
    if r.check('the window goes to its smallest size', resize(r, 1024, 640)):
        time.sleep(0.5)
        r.shot('music-1024')
    r.check('no error toasts', not r.errors(), r.errors())


@flow('youtube-missing', 'A missing yt-dlp shows how to install it and adds nothing', before=missing)
def youtube_missing(r):
    music(r)
    paste(r, f'https://youtu.be/{SONG}')
    r.check('the notice still precedes lookup when yt-dlp is missing', wait(lambda: dialog(r), 5))
    answer(r, 'I understand')
    error = wait(lambda: alert(r), 10) or ''
    r.check('missing yt-dlp shows a plain not-installed message and install hint',
            'not installed' in error and 'install' in error and 'yt-dlp' in error
            and any(hint in error for hint in ('brew install yt-dlp', 'winget install yt-dlp', 'pipx install yt-dlp'))
            and 'YTDLP_MISSING:' not in error, error)
    r.check('a missing yt-dlp adds no asset or clip', r.state()['assets'] == [] and all(not t['clips'] for t in r.state()['tracks']))
    r.shot('missing-ytdlp')
