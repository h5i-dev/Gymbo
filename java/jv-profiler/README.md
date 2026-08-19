# jv profile

Where `mvn` spends the time before it does anything you asked for.

Maven's build summary times each module and nothing else, so the part a
developer actually waits through — reading POMs, building effective models,
resolving plugins, resolving dependencies — is invisible. That is exactly the
part a faster resolver would replace, and it cannot be argued about without a
number.

## Use

    java/jv-profiler/build.sh /path/to/maven
    mvn -Dmaven.ext.class.path=.../jv-profiler.jar test

Nothing is consumed, altered or delayed; the spy only observes.

## What it found

The reason this exists was to decide whether to build a Maven core extension
that replaces Aether with jv's resolver — the "same command, much faster"
shape that made uv worth using. Measured on maven-surefire, 26 reactor
modules, warm cache, offline, three runs:

    reactor: read POMs, build models, sort     ~272ms   14%
    dependency + plugin resolution              ~68ms    3.5%   (803 artifacts)
    mojo execution                            ~1254ms   65%
    total                                     ~1900ms

**Resolution is 3.5% of the build.** Replacing it with something infinitely
fast saves 3.5%. Taking model building too — which an Aether replacement does
not reach, since Maven builds models with its own code — caps the prize at 17%.

So the extension is not worth building, and this tool is the reason we know
that rather than the reason we hoped otherwise. Warm Aether resolution is
reading local files, and it is already fast; Maven's cost is elsewhere.

The numbers that remain interesting are the cold ones: on a cache miss the
download dominates, which is the case `jv sync` addresses.

## Caveats

`validate` was used because it is the only goal that completes offline across
the whole reactor here. A full `mvn test` resolves test classpaths as well, so
its resolution share will be higher than 3.5% — but resolution warm is local
file reads, so not by the order of magnitude the extension would need.

Sums for resolution and mojos are over spans, so with `-T` they overlap and can
exceed wall clock. The reactor figure is wall clock and never overlaps.
