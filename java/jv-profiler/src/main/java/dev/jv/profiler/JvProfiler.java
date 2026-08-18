package dev.jv.profiler;

import java.io.PrintStream;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.atomic.AtomicLong;

import org.apache.maven.eventspy.AbstractEventSpy;
import org.apache.maven.execution.ExecutionEvent;
import org.eclipse.aether.RepositoryEvent;

/**
 * Reports where {@code mvn} spends the time before it does anything useful.
 *
 * <p>Maven's own build summary times each module's mojos and nothing else, so
 * the part a developer actually waits through — reading POMs, building
 * effective models, resolving plugins, resolving dependencies — is invisible.
 * That is the part a faster resolver could replace, and it cannot be argued
 * about without a number.
 *
 * <p>Installed as an {@code EventSpy}, which Maven loads from
 * {@code -Dmaven.ext.class.path} before the build starts. Everything here is
 * measurement: no event is consumed, altered, or delayed.
 */
public class JvProfiler extends AbstractEventSpy {

    /** When this spy was constructed, which is as close to "mvn started" as a spy can see. */
    private final long started = System.nanoTime();

    /** When the session began executing, i.e. when model building and reactor construction finished. */
    private volatile long sessionStarted = -1;

    /** Aether resolution, summed over artifacts. */
    private final AtomicLong artifactResolveNanos = new AtomicLong();
    private final AtomicLong artifactCount = new AtomicLong();
    private final AtomicLong artifactDownloadNanos = new AtomicLong();
    private final AtomicLong artifactDownloadCount = new AtomicLong();
    private final AtomicLong metadataResolveNanos = new AtomicLong();
    private final AtomicLong metadataCount = new AtomicLong();

    /** Mojo execution, which is the work rather than the overhead. */
    private final AtomicLong mojoNanos = new AtomicLong();
    private final AtomicLong mojoCount = new AtomicLong();

    private final AtomicLong projectCount = new AtomicLong();

    /** Start stamps for spans, keyed by the thing being resolved. */
    private final Map<Object, Long> pending = new ConcurrentHashMap<>();

    /** Per-mojo totals, for naming the worst offenders. */
    private final Map<String, AtomicLong> byMojo = new ConcurrentHashMap<>();

    @Override
    public void onEvent(Object event) {
        if (event instanceof ExecutionEvent) {
            onExecutionEvent((ExecutionEvent) event);
        } else if (event instanceof RepositoryEvent) {
            onRepositoryEvent((RepositoryEvent) event);
        }
    }

    private void onExecutionEvent(ExecutionEvent event) {
        ExecutionEvent.Type type = event.getType();
        if (type == null) {
            return;
        }
        switch (type) {
            case SessionStarted:
                // Everything before this is reading POMs, building effective
                // models and sorting the reactor. It is the number this tool
                // exists to produce.
                sessionStarted = System.nanoTime();
                break;
            case ProjectStarted:
                projectCount.incrementAndGet();
                break;
            case MojoStarted:
                pending.put(mojoKey(event), System.nanoTime());
                break;
            case MojoSucceeded:
            case MojoFailed:
            case MojoSkipped: {
                Long start = pending.remove(mojoKey(event));
                if (start != null) {
                    long elapsed = System.nanoTime() - start;
                    mojoNanos.addAndGet(elapsed);
                    mojoCount.incrementAndGet();
                    byMojo.computeIfAbsent(mojoName(event), key -> new AtomicLong())
                            .addAndGet(elapsed);
                }
                break;
            }
            default:
                break;
        }
    }

    private void onRepositoryEvent(RepositoryEvent event) {
        RepositoryEvent.EventType type = event.getType();
        if (type == null) {
            return;
        }
        switch (type) {
            case ARTIFACT_RESOLVING:
                pending.put(resolveKey("a", event), System.nanoTime());
                break;
            case ARTIFACT_RESOLVED:
                record(resolveKey("a", event), artifactResolveNanos, artifactCount);
                break;
            case ARTIFACT_DOWNLOADING:
                pending.put(resolveKey("d", event), System.nanoTime());
                break;
            case ARTIFACT_DOWNLOADED:
                record(resolveKey("d", event), artifactDownloadNanos, artifactDownloadCount);
                break;
            case METADATA_RESOLVING:
                pending.put(resolveKey("m", event), System.nanoTime());
                break;
            case METADATA_RESOLVED:
                record(resolveKey("m", event), metadataResolveNanos, metadataCount);
                break;
            default:
                break;
        }
    }

    private void record(Object key, AtomicLong total, AtomicLong count) {
        Long start = pending.remove(key);
        if (start != null) {
            total.addAndGet(System.nanoTime() - start);
            count.incrementAndGet();
        }
    }

    private Object mojoKey(ExecutionEvent event) {
        return "mojo:" + System.identityHashCode(event.getMojoExecution())
                + ":" + (event.getProject() == null ? "" : event.getProject().getId());
    }

    private String mojoName(ExecutionEvent event) {
        if (event.getMojoExecution() == null) {
            return "unknown";
        }
        return event.getMojoExecution().getArtifactId() + ":" + event.getMojoExecution().getGoal();
    }

    private Object resolveKey(String kind, RepositoryEvent event) {
        Object subject = event.getArtifact() != null ? event.getArtifact() : event.getMetadata();
        return kind + ":" + String.valueOf(subject);
    }

    @Override
    public void close() {
        long total = System.nanoTime() - started;
        long beforeSession = sessionStarted > 0 ? sessionStarted - started : -1;

        PrintStream out = System.out;
        out.println();
        out.println("jv profile - where mvn spent its time");
        out.println("------------------------------------------------------------");
        line(out, "reactor: read POMs, build models, sort", beforeSession, total);
        line(out, "  (" + projectCount.get() + " modules)", -1, -1);
        line(out, "dependency + plugin resolution", artifactResolveNanos.get(), total);
        line(out, "  of which downloaded (" + artifactDownloadCount.get() + ")",
                artifactDownloadNanos.get(), total);
        line(out, "  artifacts resolved: " + artifactCount.get(), -1, -1);
        line(out, "metadata resolution (" + metadataCount.get() + ")",
                metadataResolveNanos.get(), total);
        line(out, "mojo execution (" + mojoCount.get() + " executions)", mojoNanos.get(), total);
        out.println("------------------------------------------------------------");
        line(out, "observed by this spy", total, total);
        out.println();
        out.println("Resolution and mojo totals are sums over spans; with -T they");
        out.println("overlap, so they can exceed wall clock. The reactor figure is");
        out.println("wall clock and never overlaps.");

        List<Map.Entry<String, AtomicLong>> worst = new ArrayList<>(byMojo.entrySet());
        worst.sort((left, right) -> Long.compare(right.getValue().get(), left.getValue().get()));
        if (!worst.isEmpty()) {
            out.println();
            out.println("slowest mojos");
            Map<String, Long> shown = new LinkedHashMap<>();
            for (int index = 0; index < Math.min(5, worst.size()); index++) {
                shown.put(worst.get(index).getKey(), worst.get(index).getValue().get());
            }
            for (Map.Entry<String, Long> entry : shown.entrySet()) {
                line(out, "  " + entry.getKey(), entry.getValue(), total);
            }
        }
        out.println();
    }

    private void line(PrintStream out, String label, long nanos, long total) {
        if (nanos < 0) {
            out.printf("%-46s%n", label);
            return;
        }
        double millis = nanos / 1_000_000.0;
        if (total > 0) {
            out.printf("%-46s %8.0fms  %5.1f%%%n", label, millis, 100.0 * nanos / total);
        } else {
            out.printf("%-46s %8.0fms%n", label, millis);
        }
    }
}
