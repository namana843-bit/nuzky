import base64
import io
import json
import math
import struct
import subprocess

from PIL import Image, ImageChops, ImageStat

from e2e.harness import export, flow, preview_crop, preview_rect, wait


def box(kind, data):
    return struct.pack('>I4s', len(data) + 8, kind.encode()) + data


def full(kind, data, version=0, flags=0):
    return box(kind, bytes([version]) + flags.to_bytes(3, 'big') + data)


def boxes(data, skip=0):
    while skip < len(data):
        size, kind = struct.unpack_from('>I4s', data, skip)
        header = 16 if size == 1 else 8
        size = struct.unpack_from('>Q', data, skip + 8)[0] if size == 1 else size or len(data) - skip
        if size < header or skip + size > len(data):
            raise ValueError('Invalid MP4 box')
        yield kind.decode(), data[skip + header:skip + size], data[skip:skip + size]
        skip += size


def heic(path, source, columns, rows, width, height, rotation=0, mirror=None, hlg=False):
    mp4 = path.with_suffix('.tiles.mp4')
    colour = ':colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc' if hlg else ''
    pixel = 'yuv420p10le' if hlg else 'yuv420p'
    filters = f'pad={columns * 512}:{rows * 512},untile={columns}x{rows}'
    if hlg:
        filters += ',scale=in_range=pc:out_color_matrix=bt2020nc:out_range=tv'
    subprocess.run(['ffmpeg', '-v', 'error', '-y', '-i', str(source), '-vf', filters, '-c:v', 'libx265',
                    '-pix_fmt', pixel, '-profile:v', 'main10' if hlg else 'main', '-crf', '4', '-x265-params',
                    'keyint=1:min-keyint=1:bframes=0:log-level=error:pools=2' + colour,
                    '-tag:v', 'hvc1', str(mp4)], check=True)
    data = mp4.read_bytes()
    table = data
    for kind in ('moov', 'trak', 'mdia', 'minf', 'stbl'):
        table = next(payload for name, payload, _ in boxes(table) if name == kind)
    stsd = next(payload for name, payload, _ in boxes(table) if name == 'stsd')
    visual = next(payload for name, payload, _ in boxes(stsd, 8) if name == 'hvc1')
    hvcc = next(raw for name, _, raw in boxes(visual, 78) if name == 'hvcC')
    packets = json.loads(subprocess.run(['ffprobe', '-v', 'error', '-show_entries', 'packet=pos,size', '-of', 'json', str(mp4)],
                                        capture_output=True, text=True, check=True).stdout)['packets']
    samples = [data[int(p['pos']):int(p['pos']) + int(p['size'])] for p in packets]
    mp4.unlink()
    assert len(samples) == columns * rows, (len(samples), columns, rows)
    ids = list(range(2, len(samples) + 2))
    descriptor = struct.pack('>BBBBHH', 0, 0, rows - 1, columns - 1, width, height)
    properties = [hvcc, full('ispe', struct.pack('>II', 512, 512)), full('ispe', struct.pack('>II', width, height))]
    associations = [3]
    for kind, value in (('irot', rotation if rotation else None), ('imir', mirror)):
        if value is not None:
            properties.append(box(kind, bytes([value])))
            associations.append(0x80 | len(properties))
    ipma = struct.pack('>HB', 1, len(associations)) + bytes(associations)
    ipma += b''.join(struct.pack('>HB', i, 2) + bytes([0x81, 2]) for i in ids)
    info = full('infe', struct.pack('>HH4s', 1, 0, b'grid') + b'\0', 2)
    info += b''.join(full('infe', struct.pack('>HH4s', i, 0, b'hvc1') + b'\0', 2, 1) for i in ids)
    ftyp = box('ftyp', b'heic' + bytes(4) + b'mif1heic')

    def meta(offset):
        locations = struct.pack('>HHHHII', 1, 1, 0, 1, 0, len(descriptor))
        for i, sample in zip(ids, samples):
            locations += struct.pack('>HHHHII', i, 0, 0, 1, offset, len(sample))
            offset += len(sample)
        return full('meta', full('hdlr', bytes(4) + b'pict' + bytes(12) + b'\0')
                    + full('pitm', struct.pack('>H', 1)) + full('iinf', struct.pack('>H', len(ids) + 1) + info)
                    + full('iref', box('dimg', struct.pack('>HH', 1, len(ids)) + struct.pack(f'>{len(ids)}H', *ids)))
                    + box('iprp', box('ipco', b''.join(properties)) + full('ipma', struct.pack('>I', len(ids) + 1) + ipma))
                    + full('iloc', b'\x44\x00' + struct.pack('>H', len(ids) + 1) + locations, 1)
                    + box('idat', descriptor))

    path.write_bytes(ftyp + meta(len(ftyp) + len(meta(0)) + 8) + box('mdat', b''.join(samples)))


def picture(image):
    # The preview crop can take a column of the grey panel beside the canvas.
    image = image.convert('RGB').crop((1, 1, image.width - 1, image.height - 1))
    red, green, blue = image.split()
    bounds = ImageChops.lighter(ImageChops.lighter(red, green), blue).point(lambda v: 255 if v > 20 else 0).getbbox()
    return image.crop(bounds) if bounds else image


def comparison(image, reference):
    image = picture(image)
    aspect = abs((image.width / image.height) / (reference.width / reference.height) - 1)
    a, b = (im.convert('RGB').resize((32, 48), Image.Resampling.LANCZOS) for im in (image, reference))
    x, y = (list(im.convert('L').getdata()) for im in (a, b))
    mx, my = sum(x) / len(x), sum(y) / len(y)
    covariance = sum((i - mx) * (j - my) for i, j in zip(x, y))
    variance = math.sqrt(sum((i - mx) ** 2 for i in x) * sum((j - my) ** 2 for j in y))
    correlation = covariance / variance if variance else 0
    difference = sum(ImageStat.Stat(ImageChops.difference(a, b)).mean) / 3
    return aspect, correlation, difference


def check_picture(r, name, image, reference, hlg):
    aspect, correlation, difference = comparison(image, reference)
    r.check(f'{name}: the whole upright picture has the right proportions', aspect < 0.02,
            f'aspect error {aspect:.2%}; detected picture {picture(image).size}; reference {reference.size}')
    r.check(f'{name}: the picture matches FFmpeg', correlation >= 0.95,
            f'greyscale Pearson {correlation:.4f}, required 0.95')
    if not hlg:
        # The fixture's RGB error is below 1; 12 also allows thumbnail scaling and a small 720p H.264 export.
        r.check(f'{name}: SDR colours match FFmpeg', difference < 12, f'mean RGB error {difference:.2f}/255, allowed 12')


THUMB = """[...document.querySelectorAll('[title]')].find(e => e.title.split('\\n')[0] === arguments[0])?.querySelector('img')"""


@flow('heic', 'An iPhone HEIC photo made of tiles imports as one whole upright picture in the preview, its thumbnail and the export')
def heic_flow(r):
    fixtures = []
    for name, columns, rows, width, height, rotation, mirror, hlg in (
            ('IMG_4032.HEIC', 8, 6, 4032, 3024, 3, None, False), ('freeze.heic', 3, 4, 1080, 1920, 0, 1, True)):
        source, path, ref = r.work / f'{name}-source.png', r.work / name, r.work / f'{name}-ref.png'
        subprocess.run(['ffmpeg', '-v', 'error', '-y', '-f', 'lavfi', '-i', f'testsrc2=size={width}x{height}:rate=1',
                        '-vf', 'drawbox=x=0:y=0:w=iw/3:h=ih/3:color=white:t=fill', '-frames:v', '1', str(source)], check=True)
        heic(path, source, columns, rows, width, height, rotation, mirror, hlg)
        subprocess.run(['ffmpeg', '-v', 'error', '-y', '-i', str(path), '-frames:v', '1', str(ref)], check=True)
        fixtures.append((path, Image.open(ref).convert('RGB'), hlg))

    r.s.run("""window.__heicRaf = {last: performance.now(), max: 0, frames: 0, active: true};
        const tick = now => { const s = window.__heicRaf; if (!s.active) return;
          s.max = Math.max(s.max, now - s.last); s.last = now; s.frames++; requestAnimationFrame(tick); };
        requestAnimationFrame(tick);""")
    dropped = r.s.call("""(() => {
        const b = document.querySelector('section[aria-label=Timeline] [role=listbox][aria-label="Main track"]').getBoundingClientRect();
        const d = window.devicePixelRatio || 1;
        return window.__TAURI_INTERNALS__.invoke('plugin:event|emit', {event: 'tauri://drag-drop',
          payload: {paths: arguments[0], position: {x: (b.left + 2) * d, y: (b.top + b.height / 2) * d}}});
        })()""", [str(p) for p, _, _ in fixtures])
    imported = dropped['ok'] and wait(lambda: len(r.state()['assets']) == 2 and len(r.track()) == 2, 30)
    ready = False
    if imported:
        first = r.track()[0]
        r.seek(first['startUs'] + first['durationUs'] // 2)
        ready = wait(lambda: r.s.run("return document.querySelector('section[aria-label=Preview] canvas').dataset.us")
                     == str(first['startUs'] + first['durationUs'] // 2)
                     and all(r.s.run(f'const i = {THUMB}; return !!(i?.complete && i.naturalWidth)', p.name)
                             for p, _, _ in fixtures), 30)
    timing = r.s.run("""const s = window.__heicRaf; s.active = false;
        return {max: Math.max(s.max, performance.now() - s.last), frames: s.frames};""")
    r.shot('heic-drop')
    r.check('both OS-dropped HEIC files import and are placed without an error', imported and not r.errors(),
            {'emit': dropped, 'state': r.state(), 'errors': r.errors()})
    r.check('the page keeps drawing through first preview and thumbnail decoding', ready and timing['frames'] > 0 and timing['max'] < 250,
            f"first frame and thumbnails ready={bool(ready)}; longest rAF gap {timing['max']:.1f} ms; {timing['frames']} frames")
    if not imported:
        return
    assets = r.s.run('return window.__nuzky.store.getState().snap.project.assets')
    middle = {}
    for path, reference, hlg in fixtures:
        asset = next(a for a in assets if a['name'] == path.name)
        r.check(f'{path.name}: an image with its upright dimensions', asset['kind'] == 'image'
                and (asset['width'], asset['height']) == reference.size,
                f"{asset['kind']} {asset['width']}x{asset['height']}; expected image {reference.size}")
        clip = next(c for c in r.track() if c['assetId'] == asset['id'])
        middle[path.name] = at = clip['startUs'] + clip['durationUs'] // 2
        r.seek(at)
        drawn = wait(lambda: r.s.run("return document.querySelector('section[aria-label=Preview] canvas').dataset.us") == str(at), 15)
        r.check(f'{path.name}: the preview draws the requested frame', drawn, f'at {at} us')
        r.shot(f'{path.name}-preview')
        check_picture(r, f'{path.name} preview', preview_crop(r.work / f'{path.name}-preview.png', preview_rect(r)), reference, hlg)
        loaded = wait(lambda: r.s.run(f'const i = {THUMB}; return !!(i?.complete && i.naturalWidth)', path.name), 15)
        r.check(f'{path.name}: the Media tile displays a thumbnail', loaded, path.name)
        if loaded:
            thumb = r.s.call(f"""(() => {{ const i = {THUMB}; return fetch(i.currentSrc).then(v => v.blob()).then(b =>
                new Promise((resolve, reject) => {{ const reader = new FileReader(); reader.onload = () => resolve(reader.result);
                  reader.onerror = () => reject(reader.error); reader.readAsDataURL(b); }})); }})()""", path.name)
            r.check(f'{path.name}: the displayed thumbnail can be decoded', thumb['ok'], thumb.get('error'))
            if thumb['ok']:
                check_picture(r, f'{path.name} thumbnail', Image.open(io.BytesIO(base64.b64decode(thumb['value'].split(',', 1)[1]))), reference, hlg)
    video = export(r, 'heic.mp4')
    if video.exists():
        for path, reference, hlg in fixtures:
            frame = r.work / f'{path.name}-export.png'
            subprocess.run(['ffmpeg', '-v', 'error', '-y', '-ss', str(middle[path.name] / 1e6), '-i', str(video),
                            '-frames:v', '1', str(frame)], check=True)
            check_picture(r, f'{path.name} export', Image.open(frame), reference, hlg)
    r.check('nothing shows an error', not r.errors(), r.errors())
