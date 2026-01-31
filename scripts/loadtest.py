#!/usr/bin/env python3
"""
Load test script for rstmdb server.

Usage:
    # Basic test (100 instances, 10 events each)
    ./scripts/loadtest.py

    # Custom load
    ./scripts/loadtest.py --instances 1000 --events 50 --concurrency 20

    # With authentication
    ./scripts/loadtest.py --token my-secret-token

    # TLS
    ./scripts/loadtest.py --tls --tls-insecure
"""

import argparse
import asyncio
import random
import statistics
import string
import sys
import time
from dataclasses import dataclass, field
from typing import Optional

# Add the python client to path if running from repo
sys.path.insert(0, "../rstmdb-py/src")

try:
    from rstmdb import Client
except ImportError:
    print("Error: rstmdb Python client not found.")
    print("Install it or run from the rstmdb-py directory:")
    print("  pip install -e ../rstmdb-py")
    sys.exit(1)


@dataclass
class Stats:
    """Collected statistics."""
    operation: str
    count: int = 0
    errors: int = 0
    latencies: list[float] = field(default_factory=list)
    start_time: float = 0
    end_time: float = 0

    def record(self, latency: float, error: bool = False):
        self.count += 1
        if error:
            self.errors += 1
        else:
            self.latencies.append(latency)

    def report(self) -> dict:
        if not self.latencies:
            return {
                "operation": self.operation,
                "count": self.count,
                "errors": self.errors,
                "throughput": 0,
            }

        duration = self.end_time - self.start_time
        sorted_lat = sorted(self.latencies)

        return {
            "operation": self.operation,
            "count": self.count,
            "errors": self.errors,
            "throughput": len(self.latencies) / duration if duration > 0 else 0,
            "latency_ms": {
                "min": min(sorted_lat) * 1000,
                "max": max(sorted_lat) * 1000,
                "mean": statistics.mean(sorted_lat) * 1000,
                "p50": sorted_lat[len(sorted_lat) // 2] * 1000,
                "p90": sorted_lat[int(len(sorted_lat) * 0.9)] * 1000,
                "p99": sorted_lat[int(len(sorted_lat) * 0.99)] * 1000 if len(sorted_lat) >= 100 else sorted_lat[-1] * 1000,
            }
        }


def random_string(length: int = 8) -> str:
    return ''.join(random.choices(string.ascii_lowercase + string.digits, k=length))


async def setup_machine(client: Client, machine_name: str) -> None:
    """Create the test state machine with a cycle for continuous event application."""
    await client.put_machine(
        machine_name,
        1,
        {
            "states": ["s0", "s1", "s2", "s3", "s4"],
            "initial": "s0",
            "transitions": [
                {"from": "s0", "event": "NEXT", "to": "s1"},
                {"from": "s1", "event": "NEXT", "to": "s2"},
                {"from": "s2", "event": "NEXT", "to": "s3"},
                {"from": "s3", "event": "NEXT", "to": "s4"},
                {"from": "s4", "event": "NEXT", "to": "s0"},  # Cycle back
            ],
        },
    )


async def worker(
    worker_id: int,
    host: str,
    port: int,
    token: Optional[str],
    tls: bool,
    tls_insecure: bool,
    machine_name: str,
    num_instances: int,
    events_per_instance: int,
    stats: dict[str, Stats],
    semaphore: asyncio.Semaphore,
) -> None:
    """Worker that creates instances and applies events."""
    async with semaphore:
        try:
            async with Client(
                host, port,
                token=token,
                tls=tls,
                insecure=tls_insecure,
            ) as client:
                for i in range(num_instances):
                    instance_id = f"loadtest-{worker_id}-{i}-{random_string()}"

                    # Create instance
                    start = time.perf_counter()
                    try:
                        await client.create_instance(
                            machine=machine_name,
                            version=1,
                            instance_id=instance_id,
                            initial_ctx={"worker": worker_id, "index": i},
                        )
                        stats["create"].record(time.perf_counter() - start)
                    except Exception as e:
                        stats["create"].record(time.perf_counter() - start, error=True)
                        print(f"Worker {worker_id}: create error: {e}")
                        continue

                    # Apply events (using cyclic state machine)
                    for _ in range(events_per_instance):
                        start = time.perf_counter()
                        try:
                            await client.apply_event(
                                instance_id=instance_id,
                                event="NEXT",
                                payload={"timestamp": time.time()},
                            )
                            stats["apply_event"].record(time.perf_counter() - start)
                        except Exception as e:
                            stats["apply_event"].record(time.perf_counter() - start, error=True)
                            # State machine might not allow this transition, that's ok
                            break

                    # Get instance
                    start = time.perf_counter()
                    try:
                        await client.get_instance(instance_id)
                        stats["get"].record(time.perf_counter() - start)
                    except Exception as e:
                        stats["get"].record(time.perf_counter() - start, error=True)

                    # Delete instance (cleanup)
                    start = time.perf_counter()
                    try:
                        await client.delete_instance(instance_id)
                        stats["delete"].record(time.perf_counter() - start)
                    except Exception as e:
                        stats["delete"].record(time.perf_counter() - start, error=True)

        except Exception as e:
            print(f"Worker {worker_id} connection error: {e}")


async def run_loadtest(
    host: str,
    port: int,
    token: Optional[str],
    tls: bool,
    tls_insecure: bool,
    num_instances: int,
    events_per_instance: int,
    concurrency: int,
) -> None:
    """Run the load test."""
    machine_name = f"loadtest-{random_string()}"

    print(f"Load Test Configuration:")
    print(f"  Server: {host}:{port}")
    print(f"  TLS: {tls}")
    print(f"  Instances: {num_instances}")
    print(f"  Events per instance: {events_per_instance}")
    print(f"  Concurrency: {concurrency}")
    print(f"  Machine: {machine_name}")
    print()

    # Setup
    print("Setting up test machine...")
    async with Client(host, port, token=token, tls=tls, insecure=tls_insecure) as client:
        await setup_machine(client, machine_name)

    # Initialize stats
    stats = {
        "create": Stats("create_instance"),
        "apply_event": Stats("apply_event"),
        "get": Stats("get_instance"),
        "delete": Stats("delete_instance"),
    }

    # Distribute work across workers
    instances_per_worker = num_instances // concurrency
    remainder = num_instances % concurrency

    semaphore = asyncio.Semaphore(concurrency)

    print(f"Starting {concurrency} workers...")
    start_time = time.perf_counter()

    for s in stats.values():
        s.start_time = start_time

    tasks = []
    for w in range(concurrency):
        worker_instances = instances_per_worker + (1 if w < remainder else 0)
        if worker_instances > 0:
            tasks.append(
                worker(
                    w, host, port, token, tls, tls_insecure,
                    machine_name, worker_instances, events_per_instance,
                    stats, semaphore,
                )
            )

    await asyncio.gather(*tasks)

    end_time = time.perf_counter()
    for s in stats.values():
        s.end_time = end_time

    # Report
    total_duration = end_time - start_time
    print()
    print("=" * 60)
    print(f"Load Test Results (duration: {total_duration:.2f}s)")
    print("=" * 60)

    total_ops = 0
    total_errors = 0

    for stat in stats.values():
        report = stat.report()
        total_ops += report["count"]
        total_errors += report["errors"]

        print(f"\n{report['operation']}:")
        print(f"  Count: {report['count']} ({report['errors']} errors)")
        print(f"  Throughput: {report['throughput']:.2f} ops/sec")
        if "latency_ms" in report:
            lat = report["latency_ms"]
            print(f"  Latency (ms):")
            print(f"    min: {lat['min']:.2f}")
            print(f"    mean: {lat['mean']:.2f}")
            print(f"    p50: {lat['p50']:.2f}")
            print(f"    p90: {lat['p90']:.2f}")
            print(f"    p99: {lat['p99']:.2f}")
            print(f"    max: {lat['max']:.2f}")

    print()
    print("-" * 60)
    print(f"Total operations: {total_ops}")
    print(f"Total errors: {total_errors}")
    print(f"Overall throughput: {total_ops / total_duration:.2f} ops/sec")
    print("-" * 60)


def main():
    parser = argparse.ArgumentParser(description="Load test rstmdb server")
    parser.add_argument("--host", default="127.0.0.1", help="Server host")
    parser.add_argument("--port", type=int, default=7401, help="Server port")
    parser.add_argument("--token", "-t", help="Authentication token")
    parser.add_argument("--tls", action="store_true", help="Enable TLS")
    parser.add_argument("--tls-insecure", "-k", action="store_true", help="Skip TLS verification")
    parser.add_argument("--instances", "-n", type=int, default=100, help="Number of instances to create")
    parser.add_argument("--events", "-e", type=int, default=10, help="Events per instance")
    parser.add_argument("--concurrency", "-c", type=int, default=10, help="Number of concurrent connections")

    args = parser.parse_args()

    asyncio.run(
        run_loadtest(
            host=args.host,
            port=args.port,
            token=args.token,
            tls=args.tls,
            tls_insecure=args.tls_insecure,
            num_instances=args.instances,
            events_per_instance=args.events,
            concurrency=args.concurrency,
        )
    )


if __name__ == "__main__":
    main()
