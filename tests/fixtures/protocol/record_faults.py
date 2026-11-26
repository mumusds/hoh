#!/usr/bin/env python3
"""Provider-fault fixtures (M2T07R-256, REQ-042): what each agent CLI writes on the stream HoH reads when its provider
answers 429, 503 or a usage limit, recorded against a fake provider on 127.0.0.1.

    python3 tests/fixtures/protocol/record_faults.py [--agents claude,codex,pi]

The script re-executes itself as `sudo -n unshare -n` (loopback up, then back to the calling user with `setpriv`) and
refuses to run any CLI unless the namespace has only `lo` and no route out. Every CLI runs with a fresh HOME (and
CODEX_HOME, PI_CODING_AGENT_DIR) holding only the fake provider's configuration and a dummy key: no real provider,
account, login or credential is involved. Processes, scopes, staging and publication are record.py's: each case is
stored like a live case (stdin.txt, stdout.jsonl, stderr.log, meta.json with `mode: fake-provider` and the fake
provider's log of every request it answered) under `_provider-faults/<agent>/<version>/<case>/`, and a version
directory is published in one rename only when every case is valid. The leading `_` keeps these directories out of
compat-manifest.json and of the live recorder's cases: they are not a CLI version's verification.
"""
from __future__ import annotations

import argparse
import errno
import http.server
import importlib.util
import json
import os
import re
import socket
import sys
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE / '_provider-faults'
_spec = importlib.util.spec_from_file_location('record', HERE / 'record.py')
record = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(record)
expect, typed = record.expect, record.typed

# --- the fake provider ---------------------------------------------------------------------------------------------

NOW = re.compile(r'\{now\+(\d+)\}')
QUOTED_NOW = re.compile(r'"\{now\+(\d+)\}"')
# Request headers never logged: whatever carries a key or a session.
SECRET_HEADERS = {'authorization', 'x-api-key', 'cookie', 'proxy-authorization'}


class FakeProvider:
    """An HTTP server on 127.0.0.1 (an ephemeral port) answering from `scenario`: the first route whose method and
    path prefix match answers with its `responses` in turn, the last one repeated; anything else gets `default`.
    `{now+N}` in a header value, or as a whole JSON string in the body, becomes the answer's time plus N seconds (epoch
    seconds, a JSON number in the body). Every answer is logged with the request's model; no key is logged."""

    def __init__(self, scenario: dict):
        self.scenario = scenario
        self.log: list[dict] = []
        self.counts: dict[int, int] = {}
        self.lock = threading.Lock()
        provider = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'

            def answer(self) -> None:
                provider.answer(self)

            do_GET = do_POST = do_PUT = do_DELETE = do_PATCH = answer

            def log_message(self, *args) -> None:
                pass

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self) -> str:
        return f'http://127.0.0.1:{self.server.server_address[1]}'

    def answer(self, h: http.server.BaseHTTPRequestHandler) -> None:
        size = int(h.headers.get('content-length') or 0)
        body = h.rfile.read(size) if size else b''
        at = time.time()
        with self.lock:
            routes = self.scenario.get('routes', [])
            index = next((i for i, r in enumerate(routes)
                          if r.get('method', h.command) == h.command and h.path.startswith(r['path_prefix'])), None)
            if index is None:
                resp = self.scenario.get('default', {'status': 404, 'body': {'error': 'no route'}})
            else:
                n = self.counts.get(index, 0)
                self.counts[index] = n + 1
                responses = routes[index]['responses']
                resp = responses[min(n, len(responses) - 1)]
        data = QUOTED_NOW.sub(lambda m: str(int(at) + int(m.group(1))), json.dumps(resp.get('body', {}))).encode()
        headers = {'content-type': 'application/json'} | {k: NOW.sub(lambda m: str(int(at) + int(m.group(1))), str(v))
                                                          for k, v in resp.get('headers', {}).items()}
        try:
            model = json.loads(body).get('model') if body else None
        except (ValueError, AttributeError):
            model = None
        entry = {'at': at, 'method': h.command, 'path': h.path, 'status': resp['status'], 'headers': headers,
                 'model': model if isinstance(model, str) else None,
                 'request_headers': sorted(k.lower() for k in h.headers.keys() if k.lower() not in SECRET_HEADERS)}
        with self.lock:
            self.log.append(entry)
        # Logged before the answer is written (FLK-17): a client that has read it finds it in the log.
        h.send_response(resp['status'])
        for k, v in headers.items():
            h.send_header(k, v)
        h.send_header('content-length', str(len(data)))
        h.end_headers()
        h.wfile.write(data)

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()


# --- scenarios -----------------------------------------------------------------------------------------------------

def anthropic(status: int, headers: dict, kind: str, message: str) -> dict:
    return {'routes': [{'method': 'POST', 'path_prefix': '/v1/messages', 'responses': [
        {'status': status, 'headers': headers, 'body': {'type': 'error', 'error': {'type': kind, 'message': message}}}]}],
        'default': {'status': 404, 'body': {'type': 'error', 'error': {'type': 'not_found_error', 'message': 'fake provider: no route'}}}}


def responses_api(status: int, headers: dict, error: dict) -> dict:
    return {'routes': [{'method': 'POST', 'path_prefix': '/v1/responses', 'responses': [
        {'status': status, 'headers': headers, 'body': {'error': error}}]}],
        'default': {'status': 404, 'body': {'error': {'type': 'not_found', 'message': 'fake provider: no route'}}}}


TPM = 'Number of request tokens has exceeded your per-minute rate limit'
SCENARIOS = {
    'claude-429-long': anthropic(429, {'retry-after': '120', 'request-id': 'req_fake_429_long'}, 'rate_limit_error', TPM),
    'claude-429-short': anthropic(429, {'retry-after': '3', 'request-id': 'req_fake_429_short'}, 'rate_limit_error', TPM),
    'claude-503-long': anthropic(503, {'retry-after': '120', 'request-id': 'req_fake_503_long'}, 'overloaded_error', 'Overloaded'),
    # A subscriber's own 5-hour limit, as the anthropic-ratelimit-unified-* headers Claude Code reads say it.
    'claude-usage-limit': anthropic(429, {
        'request-id': 'req_fake_usage_limit',
        'anthropic-ratelimit-unified-status': 'rejected', 'anthropic-ratelimit-unified-reset': '{now+7200}',
        'anthropic-ratelimit-unified-representative-claim': 'five_hour',
        'anthropic-ratelimit-unified-5h-status': 'rejected', 'anthropic-ratelimit-unified-5h-reset': '{now+7200}',
        'anthropic-ratelimit-unified-5h-utilization': '1.0',
        'anthropic-ratelimit-unified-7d-status': 'allowed', 'anthropic-ratelimit-unified-7d-reset': '{now+400000}',
        'anthropic-ratelimit-unified-7d-utilization': '0.4',
        'anthropic-ratelimit-unified-overage-status': 'rejected',
        'anthropic-ratelimit-unified-overage-disabled-reason': 'org_level_disabled'},
        'rate_limit_error', "This request would exceed your account's rate limit. Please try again later."),
    # Retry-After only in the header: no "try again in Ns" text for codex to read instead.
    'codex-429': responses_api(429, {'retry-after': '2', 'x-request-id': 'req_fake_429'},
                               {'type': 'requests', 'code': 'rate_limit_exceeded', 'message': 'Rate limit reached for requests.'}),
    'codex-503': responses_api(503, {'retry-after': '2', 'x-request-id': 'req_fake_503'},
                               {'type': 'server_error', 'code': 'server_is_overloaded', 'message': 'The server is overloaded.'}),
    'codex-usage-limit': responses_api(429, {
        'x-request-id': 'req_fake_usage_limit',
        'x-codex-primary-used-percent': '100', 'x-codex-primary-window-minutes': '300', 'x-codex-primary-reset-at': '{now+7200}',
        'x-codex-secondary-used-percent': '40', 'x-codex-secondary-window-minutes': '10080', 'x-codex-secondary-reset-at': '{now+400000}'},
        {'type': 'usage_limit_reached', 'message': 'The usage limit has been reached', 'plan_type': 'pro',
         'resets_at': '{now+7200}', 'resets_in_seconds': 7200}),
    'pi-429': anthropic(429, {'retry-after': '3', 'request-id': 'req_fake_pi_429'}, 'rate_limit_error', TPM),
    'pi-503': anthropic(503, {'retry-after': '3', 'request-id': 'req_fake_pi_503'}, 'overloaded_error', 'Overloaded'),
}
# Pi with an Anthropic subscriber's rejected 5-hour limit: the claude case's answer, its own request id.
SCENARIOS['pi-usage-limit'] = json.loads(json.dumps(SCENARIOS['claude-usage-limit']).replace('req_fake_usage_limit', 'req_fake_pi_usage_limit'))
# An exhausted credit balance: a provider limit pi's own retry rule does not retry ("billing"), so the run that took the
# prompt ends in error with no auto_retry at all.
SCENARIOS['pi-billing'] = anthropic(400, {'request-id': 'req_fake_pi_billing'}, 'invalid_request_error',
                                    'Your credit balance is too low to access the Anthropic API. '
                                    'Please go to Plans & Billing to upgrade or purchase credits.')

# --- validators: each case's own check of the outcome it claims ----------------------------------------------------


def requests(meta: dict) -> list[dict]:
    return meta.get('provider', {}).get('requests', [])


def statuses(meta: dict) -> list[int]:
    return [r.get('status') for r in requests(meta)]


def sent_header(meta: dict, name: str) -> int | None:
    rs = requests(meta)
    try:
        return int(rs[0]['headers'][name]) if rs else None
    except (KeyError, ValueError):
        return None


def gaps(meta: dict) -> list[float]:
    times = [r['at'] for r in requests(meta)]
    return [b - a for a, b in zip(times, times[1:])]


def keys(value) -> set[str]:
    if isinstance(value, dict):
        return set(value) | {k for v in value.values() for k in keys(v)}
    if isinstance(value, list):
        return {k for v in value for k in keys(v)}
    return set()


RESET_KEYS = {'resetsAt', 'resets_at', 'retryAfter', 'retry_after', 'retry_after_ms', 'retryDelay', 'retry_delay_ms'}


def claude_api_error(fs: list[dict], rc, out: list[str], status: int, error: str) -> dict:
    """The final API-error assistant frame (checked for `error` and `api_error_status`) and the failed result."""
    errs = [f for f in typed(fs, 'assistant') if f.get('is_api_error_message')]
    expect(out, bool(errs), 'no API-error assistant frame')
    last = errs[-1] if errs else {}
    expect(out, last.get('error') == error, f'assistant error {last.get("error")!r}, not {error!r}')
    expect(out, last.get('api_error_status') == status, f'assistant api_error_status {last.get("api_error_status")!r}, not {status}')
    result = record.claude_result(fs, rc, out)
    expect(out, result.get('is_error') is True, 'result is not an error')
    expect(out, result.get('api_error_status') == status, f'result api_error_status {result.get("api_error_status")!r}')
    expect(out, rc not in (0, None), f'exit code {rc}')
    return last


def v_claude_429_long(fs, rc, meta, sent, ctx):
    out: list[str] = []
    claude_api_error(fs, rc, out, 429, 'rate_limit')
    expect(out, not [f for f in typed(fs, 'system') if f.get('subtype') == 'api_retry'],
           'claude retried a 429 whose Retry-After is over its 60 s limit')
    expect(out, statuses(meta) == [429], f'the fake provider answered {statuses(meta)}, not one 429')
    return out


def v_claude_429_short(fs, rc, meta, sent, ctx):
    out: list[str] = []
    claude_api_error(fs, rc, out, 429, 'rate_limit')
    retries = [f for f in typed(fs, 'system') if f.get('subtype') == 'api_retry']
    expect(out, len(retries) == 2, f'{len(retries)} api_retry frames, not 2 (CLAUDE_CODE_MAX_RETRIES=2)')
    for f in retries:
        expect(out, f.get('error_status') == 429 and f.get('error') == 'rate_limit', f'api_retry {f}')
        expect(out, isinstance(f.get('retry_delay_ms'), int) and f['retry_delay_ms'] >= 3000,
               f'api_retry waits {f.get("retry_delay_ms")} ms, less than the Retry-After of 3 s')
    expect(out, statuses(meta) == [429] * 3, f'the fake provider answered {statuses(meta)}, not three 429s')
    expect(out, all(g >= 2.9 for g in gaps(meta)), f'requests {gaps(meta)} s apart: Retry-After not waited')
    return out


def v_claude_503_long(fs, rc, meta, sent, ctx):
    out: list[str] = []
    claude_api_error(fs, rc, out, 503, 'server_error')
    expect(out, not [f for f in typed(fs, 'system') if f.get('subtype') == 'api_retry'],
           'claude retried a 503 whose Retry-After is over its 60 s limit')
    expect(out, statuses(meta) == [503], f'the fake provider answered {statuses(meta)}, not one 503')
    return out


def v_claude_usage_limit(fs, rc, meta, sent, ctx):
    out: list[str] = []
    last = claude_api_error(fs, rc, out, 429, 'rate_limit')
    reset = sent_header(meta, 'anthropic-ratelimit-unified-reset')
    expect(out, reset is not None, 'the fake provider sent no unified reset')
    events = [f['rate_limit_info'] for f in typed(fs, 'rate_limit_event') if isinstance(f.get('rate_limit_info'), dict)]
    expect(out, any(e.get('status') == 'rejected' and e.get('resetsAt') == reset for e in events),
           f'no rejected rate_limit_event with resetsAt {reset}: {events}')
    expect(out, last.get('api_error') == 'usage_limit_reached', f'api_error {last.get("api_error")!r}')
    info = (last.get('api_error_params') or {}).get('rate_limit_info') or {}
    expect(out, info.get('status') == 'rejected' and info.get('resetsAt') == reset,
           f'api_error_params.rate_limit_info {info} does not carry the reset {reset}')
    expect(out, statuses(meta) == [429], f'the fake provider answered {statuses(meta)}, not one 429')
    return out


def notifications(fs: list[dict], method: str) -> list[dict]:
    return [f.get('params', {}) for f in fs if f.get('method') == method]


def codex_failed_turn(fs: list[dict], out: list[str]) -> dict:
    done = notifications(fs, 'turn/completed')
    expect(out, len(done) == 1, f'{len(done)} turn/completed')
    turn = done[-1].get('turn', {}) if done else {}
    expect(out, turn.get('status') == 'failed', f'turn status {turn.get("status")!r}')
    error = turn.get('error') or {}
    expect(out, set(error) <= {'message', 'codexErrorInfo', 'additionalDetails', 'misalignment'},
           f'TurnError has fields beyond the schema: {sorted(error)}')
    return error


def codex_retried(fs: list[dict], out: list[str], info: dict) -> None:
    errors = notifications(fs, 'error')
    retrying = [e for e in errors if e.get('willRetry') is True]
    expect(out, len(retrying) == 5, f'{len(retrying)} error notifications with willRetry, not 5')
    expect(out, all(e.get('error', {}).get('codexErrorInfo') == info for e in retrying),
           f'retrying errors are not {info}: {[e.get("error", {}).get("codexErrorInfo") for e in retrying]}')


def v_codex_app_429(fs, rc, meta, sent, ctx):
    out: list[str] = []
    codex_retried(fs, out, {'responseStreamDisconnected': {'httpStatusCode': 429}})
    error = codex_failed_turn(fs, out)
    expect(out, error.get('codexErrorInfo') == {'responseTooManyFailedAttempts': {'httpStatusCode': 429}},
           f'codexErrorInfo {error.get("codexErrorInfo")}')
    expect(out, statuses(meta) == [429] * 6, f'the fake provider answered {statuses(meta)}, not six 429s')
    expect(out, all(g >= 1.9 for g in gaps(meta)), f'requests {gaps(meta)} s apart: Retry-After not waited')
    return out


def v_codex_app_503(fs, rc, meta, sent, ctx):
    out: list[str] = []
    codex_retried(fs, out, {'responseStreamDisconnected': {'httpStatusCode': None}})
    error = codex_failed_turn(fs, out)
    expect(out, error.get('codexErrorInfo') == 'serverOverloaded', f'codexErrorInfo {error.get("codexErrorInfo")}')
    # 30 requests: each of codex's 5 stream retries made 6 (RA-250: pinned as the 429 case pins its 6).
    expect(out, statuses(meta) == [503] * 30, f'the fake provider answered {len(statuses(meta))} requests {sorted(set(statuses(meta)))}, not 30 503s')
    expect(out, all(g >= 1.9 for g in gaps(meta)), f'requests {gaps(meta)} s apart: Retry-After not waited')
    return out


def v_codex_app_usage_limit(fs, rc, meta, sent, ctx):
    out: list[str] = []
    reset = sent_header(meta, 'x-codex-primary-reset-at')
    limits = [p.get('rateLimits', {}) for p in notifications(fs, 'account/rateLimits/updated')]
    expect(out, any((l.get('primary') or {}).get('usedPercent') == 100 and (l.get('primary') or {}).get('resetsAt') == reset
                    for l in limits), f'no account/rateLimits/updated with an exhausted primary window resetting at {reset}')
    errors = [e for e in notifications(fs, 'error') if e.get('willRetry') is False]
    expect(out, [e.get('error', {}).get('codexErrorInfo') for e in errors] == ['usageLimitExceeded'],
           f'final error notifications {errors}')
    error = codex_failed_turn(fs, out)
    expect(out, error.get('codexErrorInfo') == 'usageLimitExceeded', f'codexErrorInfo {error.get("codexErrorInfo")}')
    expect(out, statuses(meta) == [429], f'the fake provider answered {statuses(meta)}, not one 429')
    return out


def v_codex_exec_usage_limit(fs, rc, meta, sent, ctx):
    out: list[str] = []
    failed = typed(fs, 'turn.failed')
    expect(out, len(failed) == 1 and bool(failed[0].get('error', {}).get('message')), f'turn.failed {failed}')
    expect(out, not (keys(fs) & RESET_KEYS), f'exec wrote structured reset or retry fields: {sorted(keys(fs) & RESET_KEYS)}')
    expect(out, rc not in (0, None), f'exit code {rc}')
    expect(out, statuses(meta) == [429], f'the fake provider answered {statuses(meta)}, not one 429')
    return out


def pi_failed_run(fs: list[dict], meta: dict, out: list[str], status: int, retried: bool) -> None:
    """A prompt pi took (`started`) whose run ended in a provider error: with `retried`, pi's own 2/4/8 s backoff (not
    the Retry-After) and a failed auto_retry_end, else no retry at all; the final assistant message in error with no
    structured reset or retry field; and one agent_settled, not aborted, as the last frame (REQ-026: the turn's end)."""
    responses = [f for f in typed(fs, 'response') if f.get('id') == 'p1']
    expect(out, len(responses) == 1 and responses[0].get('success') is True
           and (responses[0].get('data') or {}).get('disposition') == 'started', f'prompt response {responses}')
    starts = typed(fs, 'auto_retry_start')
    ends = typed(fs, 'auto_retry_end')
    if retried:
        expect(out, [f.get('delayMs') for f in starts] == [2000, 4000, 8000],
               f'auto_retry_start delayMs {[f.get("delayMs") for f in starts]}: not pi\'s own 2/4/8 s backoff')
        expect(out, len(ends) == 1 and ends[0].get('success') is False, f'auto_retry_end {ends}')
    else:
        expect(out, not starts and not ends, f'pi retried: {len(starts)} auto_retry_start, {len(ends)} auto_retry_end')
    final = [m for m in record.pi_assistant(fs) if m.get('stopReason') == 'error']
    expect(out, bool(final), 'no assistant message ended in error')
    expect(out, all(str(m.get('errorMessage', '')).startswith(f'{status} ') for m in final),
           f'error messages {[m.get("errorMessage") for m in final]} do not start with {status}')
    expect(out, not (keys(final) & RESET_KEYS), f'the error message has structured fields: {sorted(keys(final) & RESET_KEYS)}')
    settled = typed(fs, 'agent_settled')
    expect(out, bool(settled), 'no agent_settled')
    expect(out, len(settled) == 1 and settled[0].get('aborted') is False, f'agent_settled {settled}')
    expect(out, bool(fs) and fs[-1].get('type') == 'agent_settled', f'the last frame is {fs[-1].get("type") if fs else None}')
    expect(out, statuses(meta) == [status] * (4 if retried else 1),
           f'the fake provider answered {statuses(meta)}, not {4 if retried else 1} {status}s')


def v_pi_429(fs, rc, meta, sent, ctx):
    out: list[str] = []
    pi_failed_run(fs, meta, out, 429, retried=True)
    return out


def v_pi_503(fs, rc, meta, sent, ctx):
    out: list[str] = []
    pi_failed_run(fs, meta, out, 503, retried=True)
    return out


def v_pi_usage_limit(fs, rc, meta, sent, ctx):
    out: list[str] = []
    pi_failed_run(fs, meta, out, 429, retried=True)
    expect(out, sent_header(meta, 'anthropic-ratelimit-unified-reset') is not None, 'the fake provider sent no unified reset')
    return out


def v_pi_billing(fs, rc, meta, sent, ctx):
    out: list[str] = []
    pi_failed_run(fs, meta, out, 400, retried=False)
    return out


VALIDATORS = {
    ('claude', 'fault-429-retry-after-long'): v_claude_429_long,
    ('claude', 'fault-429-retry-after-short'): v_claude_429_short,
    ('claude', 'fault-503-retry-after-long'): v_claude_503_long,
    ('claude', 'fault-usage-limit'): v_claude_usage_limit,
    ('codex', 'app-server-fault-429-retry-after'): v_codex_app_429,
    ('codex', 'app-server-fault-503-retry-after'): v_codex_app_503,
    ('codex', 'app-server-fault-usage-limit'): v_codex_app_usage_limit,
    ('codex', 'exec-fault-usage-limit'): v_codex_exec_usage_limit,
    ('pi', 'rpc-fault-429-retry-after'): v_pi_429,
    ('pi', 'rpc-fault-503-retry-after'): v_pi_503,
    ('pi', 'rpc-fault-usage-limit'): v_pi_usage_limit,
    ('pi', 'rpc-fault-billing-no-retry'): v_pi_billing,
}
record.VALIDATORS.update(VALIDATORS)  # record.Recording.save judges each case by its validator (this module's copy)

# --- the sealed namespace ------------------------------------------------------------------------------------------

# What the outer process passes to the inner one; nothing that could name a key, a login or the operator's home.
KEEP = ('PATH', 'LANG', 'LC_ALL', 'LC_CTYPE', 'TERM', 'TZ', 'USER', 'LOGNAME', 'SHELL', 'XDG_RUNTIME_DIR',
        'DBUS_SESSION_BUS_ADDRESS', 'HOH_FAULTS_PI_NODE')
# TEST-NET addresses (RFC 5737, RFC 3849): nothing answers them; a sealed namespace has no route to them.
PROBES = (('192.0.2.1', 443, socket.AF_INET), ('2001:db8::1', 443, socket.AF_INET6))


def outer_command(argv: list[str], env: dict[str, str], uid: int, gid: int, python: str) -> list[str]:
    """This script again, inside a new network namespace with only loopback up, as `uid`/`gid`."""
    passed = [f'{k}={v}' for k, v in sorted(env.items()) if k in KEEP]
    inner = ['env', '-i', *passed, python, '-I', str(Path(__file__).resolve()), '--inside', *argv]
    script = f'ip link set lo up && exec setpriv --reuid={uid} --regid={gid} --init-groups -- "$@"'
    return ['sudo', '-n', 'unshare', '-n', '--', 'sh', '-c', script, 'sh', *inner]


def interfaces() -> list[str]:
    lines = Path('/proc/net/dev').read_text().splitlines()[2:]
    return sorted(line.split(':', 1)[0].strip() for line in lines if ':' in line)


def route_out() -> list[int]:
    found = []
    for host, port, family in PROBES:
        with socket.socket(family, socket.SOCK_STREAM) as s:
            s.settimeout(3)
            found.append(s.connect_ex((host, port)))
    return found


def sealed_problems(names: list[str], connects: list[int]) -> list[str]:
    out: list[str] = []
    expect(out, names == ['lo'], f'network interfaces {names}, not only lo')
    expect(out, all(c == errno.ENETUNREACH for c in connects), f'connecting out gave {connects}, not ENETUNREACH')
    return out


# --- the cases -----------------------------------------------------------------------------------------------------

PROMPT = f'Reply with exactly {record.TOKEN} and nothing else.'


def isolated() -> tuple[dict[str, str], Path]:
    """An environment with a fresh HOME (XDG and temporary directories in it) and no key, login or config of the
    operator's: record.py's allow-list for an unauthenticated claude."""
    env = record.claude_isolated_env(record.ENV)
    return env, Path(env['HOME'])


def case(rec, name: str, scenario: str, run, notes: str, env_overrides: list[str]) -> list[dict]:
    """Run one case against a fresh fake provider and save it with the provider's log."""
    provider = FakeProvider(SCENARIOS[scenario])
    try:
        argv, sent, out, err, rc, how = run(provider.url)
    finally:
        provider.close()
    extra = {'mode': 'fake-provider', 'env_overrides': env_overrides,
             'provider': {'scenario': scenario, 'answers': SCENARIOS[scenario], 'requests': provider.log}}
    return rec.save(name, argv, sent, out, err, rc, notes, extra, stop=how)


def claude(rec) -> None:
    base = ['claude', '-p', '--output-format', 'stream-json', '--verbose', '--dangerously-skip-permissions']
    api_key = {'ANTHROPIC_API_KEY': 'sk-ant-api03-fake-key-for-a-local-fake-provider'}
    oauth = {'CLAUDE_CODE_OAUTH_TOKEN': 'sk-ant-oat01-fake-token-for-a-local-fake-provider'}

    def runner(key: dict, more: dict | None = None):
        def run(url: str):
            env, _ = isolated()
            env |= {'ANTHROPIC_BASE_URL': url, 'DISABLE_AUTOUPDATER': '1', 'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC': '1',
                    **key, **(more or {})}
            argv = record.claude_isolated_argv(base)
            out, err, rc, how = record.run(argv, PROMPT.encode(), record.workdir(), timeout=180, env=env)
            return argv, PROMPT.encode(), out, err, rc, how
        return run

    shown = ['ANTHROPIC_BASE_URL=<fake provider>', 'HOME, XDG_*_HOME and TMPDIR=<fresh empty temp dirs>',
             'DISABLE_AUTOUPDATER=1', 'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1']
    case(rec, 'fault-429-retry-after-long', 'claude-429-long', runner(api_key),
         'API key; every request answered 429 with Retry-After: 120', shown + ['ANTHROPIC_API_KEY=<dummy>'])
    case(rec, 'fault-429-retry-after-short', 'claude-429-short', runner(api_key, {'CLAUDE_CODE_MAX_RETRIES': '2'}),
         'API key; every request answered 429 with Retry-After: 3; two retries allowed to keep the case short',
         shown + ['ANTHROPIC_API_KEY=<dummy>', 'CLAUDE_CODE_MAX_RETRIES=2'])
    case(rec, 'fault-503-retry-after-long', 'claude-503-long', runner(api_key),
         'API key; every request answered 503 overloaded_error with Retry-After: 120', shown + ['ANTHROPIC_API_KEY=<dummy>'])
    case(rec, 'fault-usage-limit', 'claude-usage-limit', runner(oauth),
         'subscriber (OAuth token); every request answered 429 with the unified headers of a rejected 5-hour limit',
         shown + ['CLAUDE_CODE_OAUTH_TOKEN=<dummy>'])


CODEX_CONFIG = '''model = "gpt-fake-5"
model_provider = "fake"

[model_providers.fake]
name = "fake"
base_url = "{url}/v1"
env_key = "HOH_FAKE_PROVIDER_KEY"
wire_api = "responses"
'''


def codex_env(url: str) -> dict[str, str]:
    env, home = isolated()
    codex_home = home / '.codex'
    codex_home.mkdir()
    (codex_home / 'config.toml').write_text(CODEX_CONFIG.format(url=url))
    return env | {'CODEX_HOME': str(codex_home), 'HOH_FAKE_PROVIDER_KEY': 'fake-key-for-a-local-fake-provider'}


class AppServer(record.AppServer):
    """record.py's App Server driver in an environment of its own."""

    def __init__(self, cwd: Path, env: dict[str, str]):
        self.p = record.Proc(record.claude_isolated_argv(['codex', 'app-server']), cwd, env)
        self.next_id = 0


def codex(rec) -> None:
    def app_server(url: str):
        s = AppServer(record.workdir(), codex_env(url))
        s.start()
        thread = s.thread({'approvalPolicy': 'never', 'sandbox': 'danger-full-access'})
        s.send('turn/start', {'threadId': thread, 'input': record.text(PROMPT)})
        s.wait(record.turn_done(thread), timeout=300)
        how = s.p.stop(grace=20)
        return ['codex', 'app-server'], s.p.sent, s.p.out, s.p.err, s.p.returncode, how

    def exec_(url: str):
        argv = ['codex', 'exec', '--json', '--dangerously-bypass-approvals-and-sandbox', '-']
        out, err, rc, how = record.run(record.claude_isolated_argv(argv), PROMPT.encode(), record.workdir(), timeout=300,
                                       env=codex_env(url))
        return argv, PROMPT.encode(), out, err, rc, how

    shown = ['CODEX_HOME=<fresh temp dir> holding only config.toml: model_provider "fake" at the fake provider',
             'HOH_FAKE_PROVIDER_KEY=<dummy>', 'HOME, XDG_*_HOME and TMPDIR=<fresh empty temp dirs>']
    case(rec, 'app-server-fault-429-retry-after', 'codex-429', app_server,
         'every request answered 429 with Retry-After: 2 (header only); initialize, thread/start, one turn/start', shown)
    case(rec, 'app-server-fault-503-retry-after', 'codex-503', app_server,
         'every request answered 503 server_is_overloaded with Retry-After: 2; initialize, thread/start, one turn/start', shown)
    case(rec, 'app-server-fault-usage-limit', 'codex-usage-limit', app_server,
         'every request answered 429 usage_limit_reached with x-codex primary/secondary window headers', shown)
    case(rec, 'exec-fault-usage-limit', 'codex-usage-limit', exec_,
         'exec --json; every request answered 429 usage_limit_reached with x-codex window headers', shown)


def pi(rec) -> None:
    def run(url: str):
        env, home = isolated()
        agent = home / 'pi-agent'
        agent.mkdir()
        (agent / 'models.json').write_text(json.dumps({'providers': {'fake': {
            'baseUrl': url, 'api': 'anthropic-messages', 'apiKey': 'fake-key-for-a-local-fake-provider',
            'models': [{'id': 'fake-model'}]}}}, indent=2) + '\n')
        node = os.environ.get('HOH_FAULTS_PI_NODE')
        env |= {'PI_CODING_AGENT_DIR': str(agent), 'PI_OFFLINE': '1'}
        if node:
            env['PATH'] = f'{node}:{env.get("PATH", "")}'
        cwd = record.workdir()
        argv = ['pi', '--mode', 'rpc', '--session-dir', str(cwd / 'pi-sessions'), '--provider', 'fake', '--model', 'fake-model']
        p, how = record.pi_session(record.claude_isolated_argv(argv), cwd,
                                   [{'id': 's1', 'type': 'get_state'}, {'id': 'p1', 'type': 'prompt', 'message': PROMPT}],
                                   until='agent_settled', timeout=120, env=env)
        shown = ['pi', '--mode', 'rpc', '--session-dir', '<temp>/pi-sessions', '--provider', 'fake', '--model', 'fake-model']
        return shown, p.sent, p.out, p.err, p.returncode, how

    shown = ['PI_CODING_AGENT_DIR=<fresh temp dir> holding only models.json', 'PI_OFFLINE=1',
             'HOME, XDG_*_HOME and TMPDIR=<fresh empty temp dirs>', "PATH: pi's own node first"]
    common = 'models.json provider "fake" (anthropic-messages) at the fake provider; '
    until = '; get_state then one prompt until agent_settled'
    case(rec, 'rpc-fault-429-retry-after', 'pi-429', run,
         common + 'every request answered 429 with Retry-After: 3' + until, shown)
    case(rec, 'rpc-fault-503-retry-after', 'pi-503', run,
         common + 'every request answered 503 overloaded_error with Retry-After: 3' + until, shown)
    case(rec, 'rpc-fault-usage-limit', 'pi-usage-limit', run,
         common + 'every request answered 429 with the unified headers of a rejected 5-hour limit' + until, shown)
    case(rec, 'rpc-fault-billing-no-retry', 'pi-billing', run,
         common + 'every request answered 400 invalid_request_error: credit balance too low (billing)' + until, shown)


AGENTS = {'claude': claude, 'codex': codex, 'pi': pi}


def record_agent(agent: str, namespace: dict) -> Path:
    """Record `agent`'s fault cases into staging and publish them; record.RecordingFailed otherwise."""
    rec = None
    version = '?'
    try:
        version = record.INSTALLED_VERSION[agent]()
        before = record.binaries(agent)
        started = record.now()
        rec = record.Recording(agent, version, ROOT)
        AGENTS[agent](rec)
        if record.INSTALLED_VERSION[agent]() != version or record.binaries(agent) != before:
            rec.fail('provenance', 'the installed binary changed during the recording')
        rec.publish({'cli_version': version, 'recording_started_at': started, 'recorded_at': record.now(),
                     'recorder': 'tests/fixtures/protocol/record_faults.py', 'namespace': namespace,
                     'provider': 'FakeProvider of record_faults.py on 127.0.0.1', **before})
        return rec.final
    except BaseException as exc:
        record.stop_all()
        where = rec.abandon() if rec is not None else 'nothing staged yet'
        if isinstance(exc, (KeyboardInterrupt, SystemExit, record.Terminated)):
            raise
        detail = str(exc) if isinstance(exc, record.RecordingFailed) else f'{type(exc).__name__}: {exc}'
        raise record.RecordingFailed(f'{agent} {version}: {detail}; published fixtures unchanged; staging kept at {where}') from exc


def inside(agents: list[str]) -> int:
    names, connects = interfaces(), route_out()
    problems = sealed_problems(names, connects)
    if problems:
        print('refusing to run any CLI: ' + '; '.join(problems), file=sys.stderr)
        return 70
    try:
        record.require_scopes()
    except RuntimeError as exc:
        print(str(exc), file=sys.stderr)
        return 69
    namespace = {'how': 'sudo -n unshare -n; ip link set lo up; setpriv back to the calling user', 'interfaces': names,
                 'route_out': 'ENETUNREACH', 'probed': [f'{h}:{p}' for h, p, _ in PROBES]}
    previous = record.install_termination_handlers()
    failures = []
    try:
        # Version and binary probes never see the operator's home either.
        record.ENV['HOME'] = str(record.tempdir('hoh-fault-home-'))
        for name in AGENTS:
            if name in agents:
                try:
                    print(f'recorded {name}: {record_agent(name, namespace)}')
                except record.RecordingFailed as exc:
                    failures.append(str(exc))
                    print(f'NOT recorded: {exc}', file=sys.stderr)
    except (KeyboardInterrupt, record.Terminated) as exc:
        print(f'interrupted ({type(exc).__name__} {exc}); published fixtures are unchanged', file=sys.stderr)
        return 130
    finally:
        try:
            record.stop_all()
        finally:
            record.restore_handlers(previous)
            if record.LIVE:
                print('warning: keeping temporary directories of children that could not be stopped: '
                      + ', '.join(map(str, record.TEMP_DIRS)), file=sys.stderr)
                record.TEMP_DIRS.clear()
            while record.TEMP_DIRS:
                record.shutil.rmtree(record.TEMP_DIRS.pop(), ignore_errors=True)
    return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--agents', default='claude,codex,pi')
    ap.add_argument('--inside', action='store_true', help=argparse.SUPPRESS)
    args = ap.parse_args()
    agents = args.agents.split(',')
    unknown = set(agents) - set(AGENTS)
    if unknown:
        ap.error('unknown agents: ' + ', '.join(sorted(unknown)))
    if args.inside:
        return inside(agents)
    env = dict(os.environ)
    node = Path(os.environ.get('XDG_DATA_HOME') or Path.home() / '.local/share') / 'pi-node/current/bin'
    if (node / 'node').exists():
        env['HOH_FAULTS_PI_NODE'] = str(node)  # pi's launcher finds its node under the HOME the runs replace
    cmd = outer_command(['--agents', args.agents], env, os.getuid(), os.getgid(), sys.executable)
    return record.subprocess.run(cmd).returncode


if __name__ == '__main__':
    sys.exit(main())
