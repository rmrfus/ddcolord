# Build and install the binary, man pages, example and user service.

PREFIX  ?= /usr/local
BINDIR  ?= $(PREFIX)/bin
MANDIR  ?= $(PREFIX)/share/man
DATADIR ?= $(PREFIX)/share/ddcolord

# systemd searches share/systemd/user for home installs and
# lib/systemd/user under /usr and /usr/local; see systemd.unit(5).
ifeq ($(filter /usr%,$(PREFIX)),)
UNITDIR ?= $(PREFIX)/share/systemd/user
else
UNITDIR ?= $(PREFIX)/lib/systemd/user
endif

CARGO   ?= cargo
INSTALL ?= install

BIN := target/release/ddcolord

.PHONY: all build install uninstall clean show

all: build

build:
	$(CARGO) build --release --locked

# Keep build separate so sudo make install does not compile as root.
install:
	@test -x '$(BIN)' || { echo 'ddcolord: $(BIN) is missing — run `make` first' >&2; exit 1; }
	$(INSTALL) -Dm755 $(BIN)                       $(DESTDIR)$(BINDIR)/ddcolord
	$(INSTALL) -Dm644 man/man1/ddcolord.1          $(DESTDIR)$(MANDIR)/man1/ddcolord.1
	$(INSTALL) -Dm644 man/man5/ddcolord.toml.5     $(DESTDIR)$(MANDIR)/man5/ddcolord.toml.5
# Side by side, so the example's ../icc/ paths resolve in place.
	$(INSTALL) -Dm644 examples/benq-pd3220u.toml   $(DESTDIR)$(DATADIR)/examples/benq-pd3220u.toml
	$(INSTALL) -d $(DESTDIR)$(DATADIR)/icc
	$(INSTALL) -m644 icc/*.icc icc/LICENSE icc/README.md $(DESTDIR)$(DATADIR)/icc/
	$(INSTALL) -d $(DESTDIR)$(UNITDIR)
	sed -e 's|^ExecStart=.*|ExecStart=$(BINDIR)/ddcolord|' \
	    systemd/ddcolord.service > $(DESTDIR)$(UNITDIR)/ddcolord.service
	chmod 644 $(DESTDIR)$(UNITDIR)/ddcolord.service
	@echo
	@echo 'ddcolord: unit    -> $(DESTDIR)$(UNITDIR)/ddcolord.service'
	@echo 'ddcolord: then    systemctl --user daemon-reload'
	@echo 'ddcolord:         systemctl --user enable --now ddcolord'

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/ddcolord
	rm -f $(DESTDIR)$(MANDIR)/man1/ddcolord.1
	rm -f $(DESTDIR)$(MANDIR)/man5/ddcolord.toml.5
	rm -f $(DESTDIR)$(DATADIR)/examples/benq-pd3220u.toml
	rm -f $(addprefix $(DESTDIR)$(DATADIR)/icc/,$(notdir $(wildcard icc/*)))
	-rmdir $(DESTDIR)$(DATADIR)/examples $(DESTDIR)$(DATADIR)/icc $(DESTDIR)$(DATADIR)
	rm -f $(DESTDIR)$(UNITDIR)/ddcolord.service

clean:
	$(CARGO) clean

# Ask where things would go without installing them: `make show PREFIX=…`.
show:
	@echo 'PREFIX  = $(PREFIX)'
	@echo 'BINDIR  = $(BINDIR)'
	@echo 'MANDIR  = $(MANDIR)'
	@echo 'DATADIR = $(DATADIR)'
	@echo 'UNITDIR = $(UNITDIR)'
