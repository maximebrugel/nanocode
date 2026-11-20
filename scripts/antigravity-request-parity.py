#!/usr/bin/env python3
import argparse
import difflib
import json
import os
import re
import shutil
import subprocess
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

parser = argparse.ArgumentParser(description='Replay one Gemini script against agy and nanocode and diff their request bodies')
parser.add_argument('--agy', type=Path, required=True)
parser.add_argument('--nanocode', type=Path, required=True)
parser.add_argument('--script', type=Path, required=True)
parser.add_argument('--prompt', required=True)
parser.add_argument('--model', default='gemini-4-neon')
parser.add_argument('--out', type=Path)
parser.add_argument('--keep', action='store_true')
args = parser.parse_args()
SCRIPT = json.loads(args.script.read_text())


def compaction(body):
    tail = json.dumps(body.get('contents', [])[-1:])
    return 'Commitments' in tail or ('summar' in tail.lower() and 'compaction' in tail.lower())


def substitute(entry, raw):
    ids = []
    for match in re.finditer(r'task id: ([^\\\s"]+)', raw):
        if match.group(1) not in ids:
            ids.append(match.group(1))
    text = json.dumps(entry)
    for index, task in enumerate(ids):
        text = text.replace('TASK%d' % index, task)
    for index, path in enumerate(re.findall(r'"WORK(/[^"]*)"', text)):
        pass
    return json.loads(text)


class Gateway(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def reply(self, payload, stream):
        if stream:
            out = b''
            parts = payload['candidates'][0]['content']['parts']
            for index, part in enumerate(parts):
                event = {'candidates': [{'content': {'role': 'model', 'parts': [part]}, 'index': 0}], 'modelVersion': payload['modelVersion']}
                if index == len(parts) - 1:
                    event['candidates'][0]['finishReason'] = 'STOP'
                    event['usageMetadata'] = payload['usageMetadata']
                out += ('data: ' + json.dumps(event) + '\r\n\r\n').encode()
            content_type = 'text/event-stream'
        else:
            out = json.dumps(payload).encode()
            content_type = 'application/json'
        self.send_response(200)
        self.send_header('content-type', content_type)
        self.send_header('content-length', str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def do_POST(self):
        server = self.server
        raw = self.rfile.read(int(self.headers.get('content-length', 0)))
        try:
            body = json.loads(raw)
        except ValueError:
            body = raw.decode('utf-8', 'replace')
        kind, tokens = 'OTHER', 100
        parts = [{'text': 'Tempo is a payments blockchain.'}]
        if isinstance(body, dict) and compaction(body):
            kind, tokens = 'COMPACT', 5000
            parts = [{'text': '<summary>\n1. Task Overview: mock\n</summary>'}]
        elif isinstance(body, dict) and (body.get('cachedContent') or (body.get('tools') and not any('googleSearch' in tool for tool in body['tools']))):
            kind = 'TURN'
            entry = SCRIPT[server.index] if server.index < len(SCRIPT) else {'parts': [{'text': 'All done.'}]}
            server.index += 1
            entry = substitute(entry, raw.decode('utf-8', 'replace').replace(server.work, 'WORK'))
            entry = json.loads(json.dumps(entry).replace('WORK', server.work))
            parts, tokens = entry['parts'], entry.get('prompt_tokens', 1000)
        server.log.append({'kind': kind, 'path': self.path, 'headers': {key.lower(): value for key, value in self.headers.items() if key.lower() not in ('authorization', 'x-goog-api-key', 'content-length', 'host')}, 'raw': raw.decode('utf-8', 'replace'), 'body': body})
        self.reply({'candidates': [{'content': {'role': 'model', 'parts': parts}, 'finishReason': 'STOP', 'index': 0}],
                    'usageMetadata': {'promptTokenCount': tokens, 'candidatesTokenCount': 10, 'cachedContentTokenCount': tokens // 2, 'totalTokenCount': tokens + 10},
                    'modelVersion': args.model}, 'stream' in self.path)


def run(name, command, env, scratch):
    home, work = scratch / 'home', scratch / 'work'
    home.mkdir(parents=True)
    work.mkdir(parents=True)
    (work / 'a.txt').write_text('hello\n')
    server = ThreadingHTTPServer(('127.0.0.1', 0), Gateway)
    server.index, server.log, server.work = 0, [], str(work)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f'http://127.0.0.1:{server.server_address[1]}'
    full = dict(PATH=os.environ['PATH'], HOME=str(home), TERM='dumb', SHELL='/bin/bash', LANG='C.UTF-8',
                AGY_LLM_GATEWAY_URL=base, AGY_LLM_GATEWAY_API_KEY='x', AGY_LLM_GATEWAY_MODELS=args.model, **env)
    done = subprocess.run(command, cwd=work, env=full, capture_output=True, text=True, timeout=600)
    server.shutdown()
    server.server_close()
    (scratch / 'stdout.txt').write_text(done.stdout)
    (scratch / 'stderr.txt').write_text(done.stderr)
    if not server.log:
        raise SystemExit(f'{name} sent no request (exit {done.returncode}):\n{done.stdout}\n{done.stderr}')
    return [scrub(entry, str(work), str(home)) for entry in server.log]


UUID = re.compile(r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}')
STAMP = re.compile(r'\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?(?:Z|[+-]\d\d:\d\d)')


def scrub(entry, work, home):
    text = json.dumps(entry, ensure_ascii=False)
    text = text.replace(work, '<WORK>').replace(home, '<HOME>')
    text = re.sub(r'<HOME>/\.nanocode/antigravity', '<APPDATA>', text)
    text = re.sub(r'<HOME>/\.gemini/antigravity-cli', '<APPDATA>', text)
    ids = {}
    text = UUID.sub(lambda match: ids.setdefault(match.group(0), '<ID%d>' % len(ids)), text)
    text = STAMP.sub('<TIME>', text)
    text = re.sub(r'Last progress: \d+s ago', 'Last progress: <N>s ago', text)
    return json.loads(text)


def lines(value):
    return json.dumps(value, indent=1, ensure_ascii=False, sort_keys=False).splitlines()


def compare(left, right):
    report = []
    turns = lambda log: [entry for entry in log if entry['kind'] in ('TURN', 'COMPACT')]
    a, b = turns(left), turns(right)
    report.append(f'requests: agy {len(a)} ({len(left)} with side calls), nanocode {len(b)} ({len(right)} with side calls)')
    for index, (x, y) in enumerate(zip(a, b)):
        report.append(f'--- request {index + 1} ({x["kind"]} vs {y["kind"]})')
        if x['path'] != y['path']:
            report.append(f'path: {x["path"]} != {y["path"]}')
        for header in sorted(set(x['headers']) | set(y['headers'])):
            if x['headers'].get(header) != y['headers'].get(header):
                report.append(f'header {header}: {x["headers"].get(header)!r} != {y["headers"].get(header)!r}')
        xb, yb = x['body'], y['body']
        if list(xb) != list(yb):
            report.append(f'body keys: {list(xb)} != {list(yb)}')
        for key in ('systemInstruction', 'labels', 'generationConfig'):
            if xb.get(key) != yb.get(key):
                diff = list(difflib.unified_diff(lines(xb.get(key)), lines(yb.get(key)), 'agy', 'nanocode', n=0, lineterm=''))
                report.append(f'{key} differs ({len(diff)} diff lines):')
                report.extend('  ' + line[:300] for line in diff[:60])
        xt = [tool['functionDeclarations'][0]['name'] for tool in xb.get('tools', [])]
        yt = [tool['functionDeclarations'][0]['name'] for tool in yb.get('tools', [])]
        same = [name for name in xt if name in yt]
        report.append(f'tools: {len(same)} shared; agy only {[n for n in xt if n not in yt]}; nanocode only {[n for n in yt if n not in xt]}')
        xd = {tool['functionDeclarations'][0]['name']: tool for tool in xb.get('tools', [])}
        yd = {tool['functionDeclarations'][0]['name']: tool for tool in yb.get('tools', [])}
        changed = [name for name in same if xd[name] != yd[name]]
        if changed:
            report.append(f'tool declarations that differ: {changed}')
        if xb.get('contents') != yb.get('contents'):
            diff = list(difflib.unified_diff(lines(xb.get('contents')), lines(yb.get('contents')), 'agy', 'nanocode', n=1, lineterm=''))
            report.append(f'contents differ ({len(diff)} diff lines):')
            report.extend('  ' + line[:400] for line in diff[:120])
        else:
            report.append('contents identical')
        prefix = lambda raw: raw[raw.find('"systemInstruction"'):raw.find('"labels"')]
        report.append('cache prefix (systemInstruction+tools bytes) ' + ('identical' if prefix(x['raw']) == prefix(y['raw']) else 'differs'))
    return report


scratch = Path(tempfile.mkdtemp())
try:
    agy = run('agy', [str(args.agy.resolve()), '-p', args.prompt, '--output-format', 'json', '--dangerously-skip-permissions'], {}, scratch / 'agy')
    ours = run('nanocode', [str(args.nanocode.resolve()), 'exec', '--ephemeral', '--no-tempo-tools', '--harness', 'antigravity', '-m', args.model, args.prompt],
               {'NANOCODE_HOME': str(scratch / 'nanocode' / 'home' / '.nanocode')}, scratch / 'nanocode')
    report = compare(agy, ours)
    print('\n'.join(report))
    if args.out:
        args.out.mkdir(parents=True, exist_ok=True)
        (args.out / 'agy.json').write_text(json.dumps(agy, indent=1, ensure_ascii=False))
        (args.out / 'nanocode.json').write_text(json.dumps(ours, indent=1, ensure_ascii=False))
        (args.out / 'report.txt').write_text('\n'.join(report) + '\n')
finally:
    if args.keep:
        print(f'kept {scratch}')
    else:
        shutil.rmtree(scratch, ignore_errors=True)
