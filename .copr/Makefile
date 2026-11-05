# Fedora COPR "make_srpm" hook: COPR runs `make -f .copr/Makefile srpm outdir=... spec=...`
# in a fresh checkout, then builds the resulting source RPM for each enabled Fedora release.
outdir ?= $(CURDIR)/dist

srpm:
	dnf -y install git cargo rpm-build tar gzip xz
	git config --global --add safe.directory '*'
	./packaging/make-srpm.sh $(outdir)
