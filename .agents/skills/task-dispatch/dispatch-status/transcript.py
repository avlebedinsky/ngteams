"""Incremental Claude transcripts and portable dispatch events, with display redaction."""

import datetime
import json
import re
import shlex
from pathlib import Path


def secret_key(key):
    key = key.lower().lstrip("-")
    return any(part in key for part in (
        "token", "secret", "passw", "pwd", "apikey", "api_key", "api-key", "credential",
        "tempauth", "code_verifier", "refresh", "cookie", "signature",
    )) or key == "sig" or key.endswith(("_sig", "-sig"))


def secret_values(text, flags=False):
    # Match only prefixes. A nonsecret enclosing assignment/flag must not hide nested secrets.
    pattern = (r'(?<!\S)(--?[\w-]+)([ \t]+)' if flags else
               r'''(?<![\w-])([\w-]+)(["']?[ \t]*[=:](?!:)[ \t]*)''')
    matcher = re.compile(pattern)
    output, copied, search = [], 0, 0
    while match := matcher.search(text, search):
        key = match[1].lower().lstrip('-')
        header = not flags and key in ('authorization', 'proxy-authorization', 'cookie', 'set-cookie')
        search = match.end()
        if not (header or secret_key(key)) or search == len(text):
            continue
        start = end = search
        quote = text[start] if text[start] in ('"', "'") else None
        if flags and text[start] == '-':
            continue
        if quote:
            start += 1
            end = start
            while end < len(text) and text[end] not in (quote, '\n', '\r'):
                end += 2 if text[end] == '\\' and end + 1 < len(text) else 1
        else:
            stops = '\r\n\"\'' if header else ' \t\r\n' if flags else ' \t\r\n&;,\'\"<>)]}'
            while end < len(text) and text[end] not in stops:
                end += 1
        if end > start:
            output.extend((text[copied:start], '[redacted]'))
            copied = search = end
    output.append(text[copied:])
    return ''.join(output)


def redact(text):
    text = secret_values(secret_values(str(text)), flags=True)
    text = re.sub(r'(?i)\bbearer[ \t]+[\w.~+/=-]{8,}', 'Bearer [redacted]', text)
    text = re.sub(r'eyJ[\w-]{6,}\.[\w-]{6,}(?:\.[\w-]*)?', '[redacted]', text)

    def opaque(match):
        value = match[0]
        mixed = bool(re.search('[a-zA-Z]', value) and re.search('[0-9]', value)) or bool(
            re.search('[a-z]', value) and re.search('[A-Z]', value)
        )
        return '[redacted]' if mixed else value

    text = re.sub(r'[A-Za-z0-9+/=]{40,}', opaque, text)
    return re.sub(r'[A-Za-z0-9_~+=-]{40,}', opaque, text)


def shown(text, limit=140):
    text = ' '.join(redact(text).split())
    return text if len(text) <= limit else text[:limit - 1].rstrip() + '…'


def timestamp(value):
    try:
        date = datetime.datetime.fromisoformat(value)
        if date.tzinfo is None:
            date = date.replace(tzinfo=datetime.UTC)
        return int(date.timestamp() * 1000)
    except (TypeError, ValueError, AttributeError, OverflowError):
        return None


def command_signals(command):
    signals = []
    # shlex respects quoted arguments: reading a script or mentioning a command is not running it.
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=';&|()\n')
        lexer.whitespace = ' \t\r'
        words, segments = [], []
        for word in lexer:
            if word and all(char in ';&|()\n' for char in word):
                segments.append(words)
                words = []
            else:
                words.append(word)
        segments.append(words)
    except ValueError:
        return signals
    for words in segments:
        while words:
            program = Path(words[0]).name
            if re.match(r'^[A-Za-z_][A-Za-z_0-9]*=', words[0]) or program in ('env', 'exec', 'time', 'command', 'setsid', 'nohup'):
                words = words[1:]
            elif program in ('nice', 'timeout', 'flock'):
                wrapper, words = program, words[1:]
                while words and words[0].startswith('-'):
                    takes_value = words[0] in ('-n', '-s', '-k', '--signal', '--kill-after', '-w', '-E', '--timeout', '--conflict-exit-code')
                    words = words[2 if takes_value else 1:]
                if wrapper != 'nice':
                    words = words[1:]
            else:
                break
        if not words:
            continue
        program, args = Path(words[0]).name, words[1:]
        if program == 'land.sh':
            signals.append('land')
        elif program == 'revmux':
            signals.append('review')
        elif program == 'curl':
            signals.append('probe')
        elif program.endswith('lint-report.sh'):
            signals.append('check')
        elif program == 'git':
            while args and args[0].startswith('-'):
                args = args[2 if args[0] in ('-C', '-c', '--git-dir', '--work-tree') else 1:]
            if args[:1] == ['commit']:
                signals.append('commit')
        elif program == 'cargo':
            subcommand = next((word for word in args if not word.startswith(('-', '+'))), '')
            if subcommand in ('test', 'clippy', 'nextest') or subcommand == 'fmt' and '--check' in args:
                signals.append('check')
            elif subcommand in ('check', 'build', 'run', 'fmt'):
                signals.append('build')
    if any(word in command for word in ('sed -i', 'cat >', 'tee ', 'git apply', 'write_text(')):
        signals.append('scratch_implement' if '/tmp/' in command else 'implement')
    return signals


def tool_activity(name, data):
    """Return a short action and phase signals without exposing tool results."""
    command = str(data.get('command', data.get('cmd', '')))
    signals = []
    if name in ('Edit', 'Write', 'MultiEdit', 'NotebookEdit'):
        path = str(data.get('file_path', data.get('notebook_path', '')))
        signals.append('scratch_implement' if path.startswith('/tmp/') else 'implement')
    if name in ('Bash', 'exec_command'):
        signals.extend(command_signals(command))
    if name == 'Skill':
        if 'revmux' in str(data.get('skill', '')):
            signals.append('review')
        elif str(data.get('skill', '')).endswith('commit'):
            signals.append('commit')
    if name in ('Bash', 'exec_command'):
        words = data.get('description') or 'Running: ' + command.split('\n')[0]
    elif name in ('Read', 'Edit', 'Write', 'MultiEdit', 'NotebookEdit'):
        verb = 'Reading' if name == 'Read' else 'Editing'
        words = verb + ' ' + str(data.get('file_path', data.get('notebook_path', 'a file')))
    elif name == 'SubagentHandback':
        words = 'Handing back its report'
    else:
        words = data.get('description') or 'Using ' + name
    return shown(words), signals


class Transcript:
    def __init__(self, path):
        self.path = Path(path)
        self.inode = None
        self.offset = 0
        self.partial = b''
        self.reset()

    def reset(self):
        self.first = self.last = self.handback = self.written = None
        self.error = self.doing = self.said = None
        self.signals = []
        self.background = {}
        self.worktree = None

    def poll(self):
        try:
            stat = self.path.stat()
            if self.inode != stat.st_ino or stat.st_size < self.offset:
                self.reset()
                self.offset = 0
                self.partial = b''
                self.inode = stat.st_ino
                try:
                    self.worktree = json.loads(self.path.with_suffix('.meta.json').read_bytes()).get('worktreePath')
                    if not isinstance(self.worktree, str):
                        self.worktree = None
                except (OSError, ValueError, AttributeError):
                    pass
            self.written = stat.st_mtime_ns // 1_000_000
            self.error = None
            with self.path.open('rb') as stream:
                stream.seek(self.offset)
                # readline bounds memory per entry; oversize entries are skipped in full.
                while chunk := stream.readline(2 * 1024 * 1024):
                    self.offset += len(chunk)
                    self.partial += chunk
                    if not chunk.endswith(b'\n'):
                        if len(self.partial) > 2 * 1024 * 1024:
                            # Keep a sentinel until the end of this oversized line.
                            self.partial = b'!' * (2 * 1024 * 1024 + 1)
                        continue
                    line, self.partial = self.partial, b''
                    if len(line) > 2 * 1024 * 1024:
                        continue
                    try:
                        entry = json.loads(line)
                        if isinstance(entry, dict):
                            self.entry(entry)
                    except (ValueError, TypeError, AttributeError):
                        continue
        except FileNotFoundError:
            # Portable event logs do not exist until their first event.
            self.error = None
        except OSError as error:
            self.error = 'The transcript cannot be read (' + type(error).__name__ + ')'

    def entry(self, entry):
        at = timestamp(entry.get('timestamp'))
        kind = entry.get('type')
        if kind == 'attachment':
            self.notifications(entry.get('attachment', {}).get('prompt', ''))
            return
        if at is None or kind not in ('dispatch', 'user', 'assistant'):
            return
        if self.first is None:
            self.first = at
        self.last = at
        if kind == 'dispatch':
            phase = entry.get('phase')
            self.signals.append((at, phase))
            self.handback = at if phase == 'done' else None
            self.doing = {'text': shown(entry.get('text', phase)), 'started_ms': at,
                          'finished_ms': at if phase == 'done' else None}
            return
        content = entry.get('message', {}).get('content', [])
        if isinstance(content, str):
            if kind == 'user':
                self.notifications(content)
            return
        for block in content if isinstance(content, list) else []:
            if not isinstance(block, dict):
                continue
            block_type = block.get('type')
            if block_type == 'text':
                if kind == 'user':
                    self.notifications(block.get('text', ''))
                else:
                    self.said = {'text': shown(block.get('text', '').lstrip('#*- >\n'), 160), 'ms': at}
            elif kind == 'assistant' and block_type == 'tool_use':
                self.tool_use(block, at)
            elif kind == 'user' and block_type == 'tool_result':
                self.tool_result(block, entry, at)

    def tool_use(self, block, at):
        name, data, call_id = block.get('name', 'a tool'), block.get('input', {}), block.get('id', '')
        if not isinstance(data, dict):
            return
        words, signals = tool_activity(name, data)
        if self.worktree:
            words = words.replace(self.worktree.rstrip('/') + '/', '')
        self.handback = at if name == 'SubagentHandback' else None
        self.signals.extend((at, signal) for signal in signals)
        self.doing = {'id': call_id, 'text': words, 'started_ms': at, 'finished_ms': None}
        if data.get('run_in_background'):
            self.background[call_id] = {'text': words, 'started_ms': at, 'task_id': None}
        if name == 'TaskStop':
            task_id = data.get('task_id', data.get('shell_id'))
            self.background = {key: val for key, val in self.background.items() if val['task_id'] != task_id}

    def tool_result(self, block, entry, at):
        call_id = block.get('tool_use_id')
        if self.doing and self.doing.get('id') == call_id:
            self.doing['finished_ms'] = at
        background = self.background.get(call_id)
        if background is None:
            return
        if block.get('is_error'):
            del self.background[call_id]
        else:
            match = re.search(r'with ID: ([\w-]+)', str(block.get('content', '')))
            background['task_id'] = entry.get('toolUseResult', {}).get('backgroundTaskId') or (match[1] if match else None)

    def notifications(self, text):
        for notice in re.findall(r'<task-notification>(.*?)</task-notification>', text, re.DOTALL):
            def tag(name, notice=notice):
                match = re.search('<' + name + '>(.*?)</' + name + '>', notice, re.DOTALL)
                return match[1].strip() if match else None
            if tag('status') == 'running':
                continue
            self.background = {
                key: val for key, val in self.background.items()
                if not (key == tag('tool-use-id') or (tag('task-id') and val['task_id'] == tag('task-id')))
            }
