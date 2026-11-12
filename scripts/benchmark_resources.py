#!/usr/bin/env python3
"""macOS comparison of two release binaries. Uses only disposable DB copies.

python3 scripts/benchmark_resources.py --before /tmp/before --after target/release/mimicmail \
  --db data/mimicmail.sqlite3 --output /tmp/resources.json
"""
import argparse
import ctypes
import datetime
import hashlib
import json
import math
import pathlib
import shutil
import socket
import sqlite3
import statistics
import struct
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


class Usage(ctypes.Structure):
    _fields_ = [('uuid', ctypes.c_uint8 * 16), ('values', ctypes.c_uint64 * 36)]


class Timebase(ctypes.Structure):
    _fields_ = [('numer', ctypes.c_uint32), ('denom', ctypes.c_uint32)]


tb = Timebase()
ctypes.CDLL('/usr/lib/libSystem.B.dylib').mach_timebase_info(ctypes.byref(tb))
lib = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
lib.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]


def usage(pid):
    value = Usage()
    if lib.proc_pid_rusage(pid, 4, ctypes.byref(value)):
        raise OSError(ctypes.get_errno())
    v = value.values
    return {'cpu_ms': (v[0] + v[1]) * tb.numer / tb.denom / 1e6,
            'footprint_mib': v[7] / 1048576, 'peak_mib': v[28] / 1048576,
            'read_bytes': v[16], 'write_bytes': v[17]}


def snapshot(source, target):
    with sqlite3.connect(source.resolve().as_uri() + '?mode=ro', uri=True) as src:
        with sqlite3.connect(target) as dst:
            src.backup(dst)


def synthetic(path, count, contacts):
    with sqlite3.connect(path) as db:
        db.executescript((pathlib.Path(__file__).resolve().parents[1] / 'sql/schema.sql').read_text())
        db.execute("ALTER TABLE mail_archive ADD COLUMN context_json TEXT NOT NULL DEFAULT '{}'")
        db.execute('PRAGMA user_version=1')
        for i in range(count):
            owner, recipient, fp = 'person:owner', f'person:recipient{i % contacts}', f'fp{i}'
            date = (datetime.datetime(2026, 1, 1, tzinfo=datetime.timezone.utc) + datetime.timedelta(minutes=i)).isoformat()
            body = f'요청 {i}\n' + '검토 결과와 일정을 전달합니다. ' * 24
            quotes = [{'sender': recipient, 'sender_name': '테스트', 'sent_at': date, 'date': date,
                       'subject': f'테스트 {i}', 'body': f'질문 {i}-{k}\n' + '참고할 내용입니다. ' * 16,
                       'key': f'quote:{i}-{k}'} for k in range(2)]
            context = {'recipient_names': {recipient: '테스트'}, 'quotes': quotes, 'references': [], 'in_reply_to': ''}
            db.execute('INSERT INTO mail_archive(owner,fingerprint,subject,sent_at,sender,to_json,cc_json,original_body,original_authored_body,source_name,context_json) VALUES(?,?,?,?,?,?,?,?,?,?,?)',
                       (owner, fp, f'테스트 {i}', date, owner, json.dumps([recipient]), '[]',
                        body + '\n' + ''.join(q['body'] for q in quotes), body, 'synthetic.eml', json.dumps(context, ensure_ascii=False)))
            db.execute('INSERT INTO sent_mail VALUES(?,?,?,?,?,?,?,?,?)', (owner, fp, f'id{i}', owner, recipient, '', 'peer', 'reply', body))


def start(binary, db):
    proc = subprocess.Popen([str(binary.resolve()), '--db', str(db), 'gui', '--port', '0', '--no-open'],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    line = proc.stdout.readline().strip()
    if not line.startswith('Mimicmail · '):
        raise RuntimeError(proc.stderr.read())
    return proc, line.split(' · ')[1]


def stop(proc):
    proc.terminate()
    try:
        proc.wait(timeout=15)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def request(base, path):
    t = time.perf_counter()
    with urllib.request.urlopen(base + path, timeout=30) as response:
        body = response.read()
    return (time.perf_counter() - t) * 1000, body


def measure(binary, dbpath, paged, repeat, idle):
    with sqlite3.connect(dbpath) as db:
        owner, recipient = db.execute('SELECT owner,recipient FROM sent_mail GROUP BY owner,recipient ORDER BY count(*) DESC LIMIT 1').fetchone()
    proc, base = start(binary, dbpath)
    out = {}
    try:
        cold_start = usage(proc.pid)
        cold_ms, _ = request(base, '/api/conversations')
        cold_end = usage(proc.pid)
        out['cold_list'] = {'elapsed_ms': cold_ms, 'cpu_ms': cold_end['cpu_ms'] - cold_start['cpu_ms'],
                            'peak_mib': cold_end['peak_mib']}
        params = {'owner': owner, 'recipient': recipient, 'compact': '1'}
        if paged:
            params['limit'] = '50'
        paths = {'list': '/api/conversations', 'search_absent': '/api/conversations?q=__absent_291__',
                 'conversation': '/api/conversation?' + urllib.parse.urlencode(params)}
        for kind, path in paths.items():
            request(base, path)
            before = usage(proc.pid)
            rows = [request(base, path) for _ in range(repeat)]
            after = usage(proc.pid)
            times = sorted(r[0] for r in rows)
            out[kind] = {'median_ms': statistics.median(times), 'p95_ms': times[math.ceil(len(times)*.95)-1],
                         'cpu_ms_per_request': (after['cpu_ms'] - before['cpu_ms']) / repeat,
                         'footprint_mib': after['footprint_mib'], 'peak_mib': after['peak_mib'],
                         'response_bytes': len(rows[-1][1])}
            if kind == 'conversation':
                out[kind]['rendered_messages'] = len(json.loads(rows[-1][1])['timeline'])
        if idle:
            before = usage(proc.pid)
            t = time.monotonic()
            time.sleep(idle)
            after = usage(proc.pid)
            out['idle'] = {'seconds': time.monotonic()-t, 'cpu_percent': (after['cpu_ms']-before['cpu_ms'])/((time.monotonic()-t)*10),
                           'footprint_mib': after['footprint_mib'], 'read_bytes': after['read_bytes']-before['read_bytes'],
                           'write_bytes': after['write_bytes']-before['write_bytes']}
    finally:
        stop(proc)
    out['db_bytes'] = dbpath.stat().st_size
    return out


def cancelled(binary, dbpath):
    # Separate cancelled-only CPU from the useful follow-up request's CPU.
    followup_path = dbpath.with_suffix('.followup.sqlite3')
    shutil.copy2(dbpath, followup_path)
    result = {}
    for followup, path in [(False, dbpath), (True, followup_path)]:
        proc, base = start(binary, path)
        port = int(base.rsplit(':', 1)[1])
        before = usage(proc.pid)
        try:
            for _ in range(2):
                stream = socket.create_connection(('127.0.0.1', port))
                stream.sendall(f'GET /api/conversations?q=__absent_291__ HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n'.encode())
                time.sleep(.05)
                stream.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack('ii', 1, 0))
                stream.close()
                time.sleep(.13)
            if followup:
                try:
                    elapsed, _ = request(base, '/api/conversations')
                    status = 200
                except urllib.error.HTTPError as error:
                    status, elapsed = error.code, None
                result.update(next_request_status=status, next_request_ms=elapsed)
            else:
                time.sleep(2)
                result['cancelled_cpu_ms'] = usage(proc.pid)['cpu_ms'] - before['cpu_ms']
        finally:
            stop(proc)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--before', required=True, type=pathlib.Path)
    parser.add_argument('--after', required=True, type=pathlib.Path)
    parser.add_argument('--db', type=pathlib.Path)
    parser.add_argument('--output', required=True, type=pathlib.Path)
    parser.add_argument('--repeat', type=int, default=20)
    parser.add_argument('--idle-seconds', type=int, default=30)
    args = parser.parse_args()
    results = {'binary_sha256': {key: hashlib.sha256(getattr(args, key).read_bytes()).hexdigest() for key in ['before', 'after']},
               'repeat': args.repeat, 'measurement': 'macOS process physical footprint; CPU milliseconds per request; HTTP wall time; synthetic mail has two quotes', 'datasets': {}}
    with tempfile.TemporaryDirectory(prefix='mimicmail-benchmark-') as folder:
        root = pathlib.Path(folder)
        datasets = []
        if args.db:
            source = root/'actual.sqlite3'
            snapshot(args.db, source)
            # Discard projections when benchmarking a DB already opened by the new GUI.
            with sqlite3.connect(source) as db:
                for name, in db.execute("SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE 'browse_%'").fetchall():
                    db.execute('DROP TRIGGER "' + name + '"')
                for name in ['browse_items','browse_contacts','browse_dirty','browse_meta']:
                    db.execute('DROP TABLE IF EXISTS ' + name)
                db.execute('VACUUM')
            datasets.append(('actual', source))
        for count, contacts in [(1000, 25), (5000, 25), (1000, 1)]:
            name = f'{count}_mails_{contacts}_contacts'
            source = root/(name+'.sqlite3')
            synthetic(source, count, contacts)
            datasets.append((name, source))
        for name, source in datasets:
            results['datasets'][name] = {}
            for version in ['before','after']:
                path = root/(name+'-'+version+'.sqlite3')
                shutil.copy2(source, path)
                out = measure(getattr(args, version), path, version=='after', args.repeat, args.idle_seconds if name==datasets[0][0] else 0)
                results['datasets'][name][version] = out
                print(name, version, json.dumps(out), flush=True)
            args.output.write_text(json.dumps(results, indent=2)+'\n')
        source = dict(datasets)['5000_mails_25_contacts']
        results['cancelled_searches'] = {}
        for version in ['before','after']:
            path = root/('cancel-'+version+'.sqlite3')
            shutil.copy2(source,path)
            results['cancelled_searches'][version] = cancelled(getattr(args,version),path)
        args.output.write_text(json.dumps(results,indent=2)+'\n')
        print('cancelled_searches',json.dumps(results['cancelled_searches']),flush=True)


if __name__ == '__main__':
    main()
