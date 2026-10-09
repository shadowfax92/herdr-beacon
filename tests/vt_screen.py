"""Minimal terminal screen for opt-in real-TUI tests (stdlib only).

Replays the bytes a Herdr client writes to its PTY into a character grid, so a
test can read what the user would see. Herdr's renderer positions every run
with CUP and draws plain text; colors, modes, and OSC queries are skipped.
Wide East Asian characters and emoji take two cells. This is not a general
terminal emulator: an unsupported sequence is ignored rather than guessed.
"""
import re

_CSI = re.compile(rb"\x1b\[([?>=<]?)([0-9;:]*)([ -/]*)([@-~])")
_OSC = re.compile(rb"\x1b\].*?(?:\x07|\x1b\\)", re.S)


def _width(ch):
    code = ord(ch)
    wide = (0x1100 <= code <= 0x115F or 0x2E80 <= code <= 0x303E or 0x3041 <= code <= 0x33FF
            or 0x3400 <= code <= 0x4DBF or 0x4E00 <= code <= 0x9FFF or 0xA000 <= code <= 0xA4CF
            or 0xAC00 <= code <= 0xD7A3 or 0xF900 <= code <= 0xFAFF or 0xFE30 <= code <= 0xFE4F
            or 0xFF00 <= code <= 0xFF60 or 0xFFE0 <= code <= 0xFFE6 or 0x1F300 <= code <= 0x1F64F
            or 0x1F900 <= code <= 0x1F9FF or 0x20000 <= code <= 0x3FFFD)
    return 2 if wide else 1


def _split_utf8(data):
    """Split off a trailing multi-byte character that a read cut in half."""
    for back in range(1, min(4, len(data)) + 1):
        lead = data[-back]
        if lead & 0xC0 == 0x80:
            continue
        need = 2 if lead & 0xE0 == 0xC0 else 3 if lead & 0xF0 == 0xE0 else 4 if lead & 0xF8 == 0xF0 else 1
        return (data, b"") if need <= back else (data[:-back], data[-back:])
    return data, b""


class Screen:
    def __init__(self, rows, cols):
        self.rows, self.cols = rows, cols
        self.grid = [[" "] * cols for _ in range(rows)]
        self.row = self.col = 0
        self.pending = b""

    def feed(self, data):
        data = self.pending + data
        # Keep an incomplete escape or UTF-8 sequence for the next chunk.
        cut = data.rfind(b"\x1b")
        if cut != -1 and not (_CSI.match(data, cut) or _OSC.match(data, cut) or
                              (len(data) > cut + 1 and data[cut + 1:cut + 2] not in b"[]")):
            data, self.pending = data[:cut], data[cut:]
        else:
            data, self.pending = _split_utf8(data)
        text_start = 0
        index = 0
        while index < len(data):
            if data[index] != 0x1B:
                index += 1
                continue
            self._text(data[text_start:index])
            csi = _CSI.match(data, index)
            osc = _OSC.match(data, index)
            if csi:
                self._csi(csi.group(1), csi.group(2), csi.group(4))
                index = csi.end()
            elif osc:
                index = osc.end()
            else:
                index += 2  # Two-byte escapes (keypad modes, charset) carry no text.
            text_start = index
        self._text(data[text_start:])

    def _csi(self, private, params, final):
        if private:
            return
        values = [int(part) if part.isdigit() else 0 for part in params.decode().replace(":", ";").split(";")] if params else []
        arg = lambda i, default: values[i] if len(values) > i and values[i] else default
        if final in b"Hf":
            self.row = min(max(arg(0, 1) - 1, 0), self.rows - 1)
            self.col = min(max(arg(1, 1) - 1, 0), self.cols - 1)
        elif final == b"J" and arg(0, 0) == 2:
            self.grid = [[" "] * self.cols for _ in range(self.rows)]
        elif final == b"J":
            for col in range(self.col, self.cols):
                self.grid[self.row][col] = " "
            for row in range(self.row + 1, self.rows):
                self.grid[row] = [" "] * self.cols
        elif final == b"K":
            start, end = {0: (self.col, self.cols), 1: (0, self.col + 1), 2: (0, self.cols)}[arg(0, 0)]
            for col in range(start, end):
                self.grid[self.row][col] = " "
        elif final == b"A":
            self.row = max(self.row - arg(0, 1), 0)
        elif final == b"B":
            self.row = min(self.row + arg(0, 1), self.rows - 1)
        elif final == b"C":
            self.col = min(self.col + arg(0, 1), self.cols - 1)
        elif final == b"D":
            self.col = max(self.col - arg(0, 1), 0)
        elif final == b"G":
            self.col = min(max(arg(0, 1) - 1, 0), self.cols - 1)
        elif final == b"d":
            self.row = min(max(arg(0, 1) - 1, 0), self.rows - 1)

    def _text(self, raw):
        for ch in raw.decode("utf-8", errors="replace"):
            if ch == "\r":
                self.col = 0
            elif ch == "\n":
                self.row = min(self.row + 1, self.rows - 1)
            elif ch == "\b":
                self.col = max(self.col - 1, 0)
            elif ch >= " ":
                width = _width(ch)
                if self.col + width > self.cols:
                    continue  # Autowrap is off in Herdr's client.
                self.grid[self.row][self.col] = ch
                if width == 2:
                    self.grid[self.row][self.col + 1] = ""
                self.col += width

    def lines(self):
        return ["".join(row) for row in self.grid]
