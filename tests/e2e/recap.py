import json, re, subprocess

from e2e.ai_panel import FAKE, agent, click, panel, send
from e2e.harness import CLI, FIXTURES, Bridge, changed_share, flow, link_models, wait
from e2e.reel import poll

DIALOG = "return document.querySelector('[role=dialog]')?.innerText ?? ''"
# The recording plays in the recap after 1.5 s of opening, its cuts under an 11 px red edge. The test pattern of
# talk.mp4 is red along its left edge and cyan along its right one, so the right edge tells.
OPENING_S = 1.5
EDGE = (1069, 400, 1080, 1500)


def setup(r):
    project = r.work / 'data/nuzky/projects/recap.nuzky'
    subprocess.run([str(CLI), 'new', str(project), str(FIXTURES / 'talk.mp4')], env=r.env, check=True, capture_output=True)
    link_models(r)
    r.env.update(NUZKY_AGENT_CLAUDE=str(FAKE), NUZKY_AGENT_CODEX=str(r.work / 'no-codex'), NUZKY_FAKE_AGENT_LOG=str(r.work / 'fake'))


def info(r, run_id):
    found = r.s.call('window.__nuzky.api.recapInfo(arguments[0], arguments[1]).then(JSON.stringify)', run_id, r.state()['epoch'])
    if not found['ok']:
        raise RuntimeError(f'recap_info failed: {found}')
    return json.loads(found['value'])


def make(r, name, run_id, end_card):
    """Exports a recap to `name` in the flow's folder through the app, as Export… does after the save dialog."""
    out = r.work / name
    started = r.s.call('window.__nuzky.api.startRecap(arguments[0], arguments[1], arguments[2], arguments[3], false)',
                       str(out), run_id, end_card, r.state()['epoch'])
    if not started['ok']:
        raise RuntimeError(f'recap {name} did not start: {started}')
    job = wait(lambda: (j := r.s.run('return window.__nuzky.store.getState().jobs[arguments[0]] ?? null', started['value']))
               and j['status'] != 'running' and j, 300)
    r.check(f'the recap {name} is made', job and job['status'] == 'done', job)
    return out


def probe(path):
    out = subprocess.run(['ffprobe', '-v', 'error', '-show_entries', 'format=duration:stream=codec_name,width,height', '-of', 'json',
                          str(path)], capture_output=True, text=True, check=True).stdout
    data = json.loads(out)
    video = next(s for s in data['streams'] if 'width' in s)
    return float(data['format']['duration']), (video['width'], video['height']), {s['codec_name'] for s in data['streams']}


def frame(r, video, at_s, name):
    """The recap's picture `at_s` seconds in, kept beside the screenshots."""
    out = r.work / f'{name}.png'
    subprocess.run(['ffmpeg', '-v', 'error', '-y', '-ss', f'{at_s:.3f}', '-i', str(video), '-frames:v', '1', str(out)], check=True)
    if out.name not in r.shots:
        r.shots.append(out.name)
    from PIL import Image
    return Image.open(out).convert('RGB')


def red_share(image, box):
    """Share of the box that is the red cuts are marked with."""
    pixels = list(image.crop(box).getdata())
    return sum(1 for (red, green, blue) in pixels if red > 200 and green < 110 and blue < 110) / max(1, len(pixels))


def recording_at(talk_us, fast_s, at_us):
    """Where the recap shows `at_us` of the recording: it plays at a constant speed after the opening."""
    return OPENING_S + at_us / talk_us * fast_s


def fast_seconds(talk_us):
    """The fast part of a recap, as crates/engine/src/recap.rs `sections` lays it out."""
    return min(talk_us, max(3_000_000, min(17_000_000, talk_us // 4))) / 1e6


@flow('recap', 'A recap of an AI run, of a run that only shortened a pause and of everything cut since the import: the run card '
      'and Export give the same numbers as the summary, and the video is 9:16, at most 30 s, marks the cuts, strikes the cut words '
      'and ends on the finished cut, with or without its nuzky.app card', before=setup)
def recap(r):
    bridge = Bridge(r, r.saved_project())
    try:
        talk_us = r.track()[0]['durationUs']
        job = poll(bridge, bridge.call('transcribe', {'language': 'en', 'model': 'small'}), 300)
        r.check('the speech is recognised', job['status'] == 'done', job)

        # The panel's agent cuts 1–3 s, words included.
        r.key('j', ctrlKey=True)
        wait(lambda: panel(r), 5)
        send(r, 'Cut the pause at the start and add captions')
        wait(lambda: agent(r)['status'] == 'idle' and 'run' in agent(r)['kinds'], 60)
        card = panel(r)['text']
        removed = next((line.strip() for line in card.splitlines() if re.match(r'Removed \d+ words?', line.strip())), None)
        lengths = re.search(r'Shortened the video from ([\d.:]+ ?s?) to ([\d.:]+ ?s?)', card)
        r.check('the run card counts the words its cut removed', removed and lengths, card)
        r.check('a run that cut something offers Export recap…', 'Export recap…' in card, card)
        r.shot('run-card')
        click(r, 'Export recap…')
        dialog = wait(lambda: (d := r.s.run(DIALOG)) and 'Export recap' in d and '→' in d and d, 20)
        r.check("Export recap opens on this run's recap with the run card's numbers",
                dialog and removed in dialog and f'{lengths[1]} →' in dialog and lengths[2] in dialog, [dialog, card])
        hints = "return [...document.querySelectorAll('div.overlay[aria-hidden]')].map((h) => h.textContent)"
        r.check('once the numbers are in, Export… is ready and no hint says it is still counting',
                wait(lambda: not any('Counting' in h for h in r.s.run(hints)), 5), r.s.run(hints))
        r.shot('recap-dialog')
        run_id = r.s.run("return window.__nuzky.agent.getState().items.find((i) => i.kind === 'run').recapRun")
        first = info(r, run_id)
        video = make(r, 'run-recap.mp4', run_id, True)
        duration, size, codecs = probe(video)
        r.check('the recap is a 1080 × 1920 H.264 and AAC video of at most 30 s, as long as the dialog said',
                size == (1080, 1920) and codecs == {'h264', 'aac'} and duration <= 30 and abs(duration - first['durationUs'] / 1e6) < 0.06,
                [duration, size, codecs, first['durationUs']])
        fast = fast_seconds(talk_us)
        cut = frame(r, video, recording_at(talk_us, fast, 2_800_000), 'recap-cut')
        kept = frame(r, video, recording_at(talk_us, fast, 6_000_000), 'recap-kept')
        r.check('the cut passage plays under a red edge and the kept one does not',
                red_share(cut, EDGE) > 0.9 and red_share(kept, EDGE) < 0.05, [red_share(cut, EDGE), red_share(kept, EDGE)])
        r.check('the cut words are struck through in the caption', red_share(cut, (60, 1250, 900, 1405)) > 0.002,
                red_share(cut, (60, 1250, 900, 1405)))
        frame(r, video, 0.7, 'recap-opening')
        # Three seconds into the finished cut, past its label, the recap shows what the renderer draws there.
        sample_s = OPENING_S + fast + 0.6
        shown = frame(r, video, sample_s + 3.0, 'recap-finished')
        drawn = r.work / 'frame-finished.png'
        subprocess.run([str(CLI), 'frame', str(r.saved_project()), '3', str(drawn), '1080'], env=r.env, check=True, capture_output=True)
        changed = changed_share(drawn, shown)
        r.check('the recap ends on the finished cut as the renderer draws it', changed < 0.01, f'{changed:.3%} of pixels differ')
        frame(r, video, duration - 0.5, 'recap-end-card')
        # A file imported after the run is a source too: its recap never replaces it.
        take = r.work / 'new-take.mp4'
        take.write_bytes((FIXTURES / 'talk.mp4').read_bytes())
        r.import_media(take)
        refused = r.s.call('window.__nuzky.api.startRecap(arguments[0], arguments[1], true, arguments[2], true)', str(take), run_id, r.state()['epoch'])
        r.check("a run's recap refuses to replace a file the project imported since",
                not refused['ok'] and take.read_bytes() == (FIXTURES / 'talk.mp4').read_bytes(), refused)
        r.s.run("document.querySelector('[role=dialog] button[aria-label^=Close], [role=dialog] button[aria-label^=Hide]')?.click()")

        # An agent in a terminal shortens a pause and nothing else.
        words = bridge.call('get_transcript', {})['words']
        gap = next(((a['end_us'], b['start_us']) for a, b in zip(words, words[1:]) if b['start_us'] - a['end_us'] >= 500_000), None)
        r.check('the transcript has a pause of half a second to cut', gap, [(w['text'], w['start_us'], w['end_us']) for w in words])
        middle = (gap[0] + gap[1]) // 2
        run = bridge.call('begin_run', {'label': 'Shorten a pause'})['run_id']
        bridge.call('apply_edits', {'run_id': run, 'request_id': 'pause', 'edits': [
            {'type': 'rippleDeleteRanges', 'ranges': [{'startUs': middle - 150_000, 'endUs': middle + 150_000}]}]})
        bridge.call('end_run', {'run_id': run, 'action': 'keep'})
        wait(lambda: not r.state()['aiRun'], 10)
        pause = info(r, run)
        r.check('a run that only shortened a pause counts one pause and no words or retakes',
                (pause['totals']['words'], pause['totals']['retakes'], pause['totals']['pauses'], pause['removed']) == (0, 0, 1, 'Removed 1 pause'),
                pause)
        r.s.run('window.__nuzky.store.setState({exportOpen: true})')
        wait(lambda: 'Export' in r.s.run(DIALOG), 10)
        click(r, 'Recap', '[role=dialog]')
        r.s.run("document.querySelector('[role=dialog] button[aria-label^=\"Shows:\"]').click()")
        listed = wait(lambda: r.s.run("return [...document.querySelectorAll('[role=menu][aria-label=Shows] [role=menuitemradio]')].map((i) => i.textContent)"), 5)
        r.check('Shows lists the runs kept in Versions, the terminal agent\'s too', listed and any(t.startswith('Shorten a pause') for t in listed), listed)
        r.s.run("[...document.querySelectorAll('[role=menu][aria-label=Shows] [role=menuitemradio]')].find((i) => i.textContent.startsWith('Shorten a pause')).click()")
        dialog = wait(lambda: (d := r.s.run(DIALOG)) and 'Removed 1 pause' in d and d, 15)
        r.check("choosing it shows that run's numbers", dialog, r.s.run(DIALOG))
        r.shot('recap-dialog-pause')
        video = make(r, 'pause-recap.mp4', run, False)
        duration, size, _ = probe(video)
        r.check('without the end card the recap is 1.5 s shorter than with it', size == (1080, 1920) and abs(duration - (pause['durationUs'] / 1e6 - 1.5)) < 0.06,
                [duration, pause['durationUs']])

        # A hand cut, then everything cut since the import.
        clips = r.track()
        hand = (clips[0]['startUs'] + 7_000_000, clips[0]['startUs'] + 8_000_000)
        edited = r.s.call("window.__nuzky.store.getState().edit({type: 'rippleDeleteRanges', ranges: [{startUs: arguments[0], endUs: arguments[1]}]})", *hand)
        r.check('the hand cut is made', edited['ok'], edited)
        now_us = wait(lambda: (d := r.s.run('return window.__nuzky.store.getState().snap.project.tracks[0].clips.reduce((e, c) => Math.max(e, c.startUs + c.durationUs), 0)'))
                      and d, 10)
        everything = info(r, None)
        r.check('everything since the import compares the whole take with the project now',
                everything['totals']['beforeUs'] == talk_us and everything['totals']['afterUs'] == now_us
                and everything['totals']['words'] >= first['totals']['words'], [everything, talk_us, now_us, first])
        r.s.run("document.querySelector('[role=dialog] button[aria-label^=\"Shows:\"]').click()")
        wait(lambda: r.s.run("return document.querySelector('[role=menu][aria-label=Shows]')"), 5)
        r.s.run("[...document.querySelectorAll('[role=menu][aria-label=Shows] [role=menuitemradio]')].find((i) => i.textContent.startsWith('Everything')).click()")
        wait(lambda: everything['before'] + ' →' in r.s.run(DIALOG), 15)
        r.shot('recap-dialog-project')
        video = make(r, 'project-recap.mp4', None, True)
        duration, size, _ = probe(video)
        r.check('the whole-project recap is 9:16 and at most 30 s', size == (1080, 1920) and duration <= 30, [duration, size])
        opening = frame(r, video, 0.7, 'project-opening')
        # The bar is the whole take; its red share is the share of the take that was cut, the run's, the pause and the hand cut.
        share, gone = red_share(opening, (60, 1040, 900, 1052)), 1 - now_us / talk_us
        r.check('the opening bar is red where the take was cut, in proportion', abs(share - gone) < 0.03, [share, gone])
        r.check('no error toast', not r.errors(), r.errors())
    finally:
        bridge.close()
