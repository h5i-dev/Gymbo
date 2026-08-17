/*
 * Copyright the jv contributors. Licensed under the Apache License 2.0.
 *
 * A test oracle: exposes Maven Resolver's own GenericVersion behavior over a
 * line protocol so jv's Rust port can be compared against it directly, rather
 * than against a human transcription of its unit tests.
 *
 * This file lives in the `org.eclipse.aether.util.version` package on purpose:
 * GenericVersion's constructor and its nested Item class are package-private,
 * and reaching them without reimplementing GenericVersionScheme's caching (and
 * therefore most of maven-resolver's api module) requires being a package peer.
 *
 * No upstream code is copied here. The classes under test are compiled straight
 * from a maven-resolver checkout via javac's -sourcepath.
 *
 * Protocol: each input line is a one-character command followed by its payload.
 *   T<version>            -> the tokenized item list, e.g. "[1, 0, alpha]"
 *   C<version>\t<version> -> "-1", "0" or "1", the sign of compareTo
 *   Q<string>             -> the detected qualifier shift, or "none"
 * One output line per input line, in order.
 */
package org.eclipse.aether.util.version;

import java.io.BufferedWriter;
import java.io.IOException;
import java.io.OutputStreamWriter;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Paths;
import java.util.List;
import java.util.Optional;

public final class Oracle {

    private Oracle() {}

    public static void main(String[] args) throws IOException {
        if (args.length != 1) {
            System.err.println("usage: Oracle <input-file>");
            System.exit(2);
        }
        List<String> lines = Files.readAllLines(Paths.get(args[0]), StandardCharsets.UTF_8);
        PrintWriter out = new PrintWriter(
                new BufferedWriter(new OutputStreamWriter(System.out, StandardCharsets.UTF_8), 1 << 16));

        for (String line : lines) {
            if (line.isEmpty()) {
                continue;
            }
            char command = line.charAt(0);
            String payload = line.substring(1);
            switch (command) {
                case 'T':
                    out.println(new GenericVersion(payload).asItems().toString());
                    break;
                case 'C': {
                    int tab = payload.indexOf('\t');
                    GenericVersion a = new GenericVersion(payload.substring(0, tab));
                    GenericVersion b = new GenericVersion(payload.substring(tab + 1));
                    int rel = a.compareTo(b);
                    out.println(rel < 0 ? "-1" : (rel > 0 ? "1" : "0"));
                    break;
                }
                case 'Q': {
                    Optional<Integer> shift = GenericQualifiers.qualifier(payload);
                    out.println(shift.isPresent() ? String.valueOf(shift.get()) : "none");
                    break;
                }
                default:
                    throw new IllegalArgumentException("unknown command: " + command);
            }
        }
        out.flush();
    }
}
