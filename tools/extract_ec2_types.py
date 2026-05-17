#!/usr/bin/env python3
"""Extract awl's compact EC2 instance type catalog from Vantage data."""

from __future__ import annotations

import argparse
import json
import sys
import urllib.request
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

DEFAULT_SOURCE = "https://instances.vantage.sh/instances.json"
DEFAULT_OUTPUT = Path("data/ec2_instance_types.json")


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate data/ec2_instance_types.json from instances.vantage.sh data."
    )
    parser.add_argument(
        "--input",
        default=DEFAULT_SOURCE,
        help="Input JSON path, URL, or '-' for stdin.",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=DEFAULT_OUTPUT,
        help="Output path for compact awl JSON.",
    )
    parser.add_argument(
        "--source",
        help="Source label recorded in output metadata; defaults to --input.",
    )
    parser.add_argument(
        "--no-pricing",
        action="store_true",
        help="Do not embed Linux on-demand prices by region.",
    )
    parser.add_argument(
        "--include-benchmarks",
        action="store_true",
        help="Include Vantage benchmark fields when present.",
    )
    args = parser.parse_args()

    source = str(args.input)
    raw = read_json(source)
    if not isinstance(raw, list):
        raise SystemExit("expected top-level Vantage JSON array")

    instances = [
        compact_instance(
            item,
            include_pricing=not args.no_pricing,
            include_benchmarks=args.include_benchmarks,
        )
        for item in raw
        if isinstance(item, dict) and isinstance(item.get("instance_type"), str)
    ]
    instances.sort(key=lambda item: str(item["instance_type"]))

    payload = {
        "source": args.source or source,
        "generated_at": datetime.now(tz=UTC).replace(microsecond=0).isoformat(),
        "count": len(instances),
        "instances": instances,
    }

    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle, separators=(",", ":"), sort_keys=True)
        handle.write("\n")
    print(f"wrote {len(instances)} instance types to {args.output}", file=sys.stderr)
    return 0


def read_json(source: str) -> Any:
    if source == "-":
        return json.load(sys.stdin)
    if source.startswith("http://") or source.startswith("https://"):
        request = urllib.request.Request(
            source,
            headers={"User-Agent": "awl-ec2-types-extractor/0.1"},
        )
        with urllib.request.urlopen(request, timeout=120) as response:
            return json.load(response)
    with Path(source).open("r", encoding="utf-8") as handle:
        return json.load(handle)


def compact_instance(
    item: dict[str, Any],
    *,
    include_pricing: bool,
    include_benchmarks: bool,
) -> dict[str, Any]:
    output: dict[str, Any] = {
        "instance_type": item["instance_type"],
    }

    copy_string(output, item, "family")
    copy_string(output, item, "pretty_name")
    copy_string(output, item, "generation")
    copy_string(output, item, "network_performance")
    copy_string(output, item, "physical_processor", "processor")
    copy_string(output, item, "clock_speed_ghz")
    copy_bool(output, item, "ebs_optimized")
    copy_bool(output, item, "enhanced_networking")
    copy_number(output, item, "vCPU", "vcpu")
    copy_number(output, item, "memory", "memory_gib")
    copy_number(output, item, "GPU", "gpu")
    copy_number(output, item, "FPGA", "fpga")
    copy_string(output, item, "GPU_model", "gpu_model")
    copy_number(output, item, "GPU_memory", "gpu_memory_gib")
    copy_number(output, item, "ebs_baseline_bandwidth", "ebs_baseline_bandwidth_mbps")
    copy_number(output, item, "ebs_baseline_iops")
    copy_number(output, item, "ebs_max_bandwidth", "ebs_max_bandwidth_mbps")
    copy_number(output, item, "ebs_iops", "ebs_max_iops")

    arch = item.get("arch")
    if isinstance(arch, list):
        output["arch"] = [value for value in arch if isinstance(value, str)]
    else:
        output["arch"] = []

    vpc = item.get("vpc")
    if isinstance(vpc, dict):
        copy_number(output, vpc, "max_enis", "vpc_max_enis")
        copy_number(output, vpc, "ips_per_eni", "vpc_ips_per_eni")

    storage = item.get("storage")
    if isinstance(storage, dict):
        copy_number(output, storage, "devices", "storage_devices")
        copy_number(output, storage, "size", "storage_size_gb")
        copy_bool(output, storage, "ssd", "storage_ssd")
        copy_bool(output, storage, "nvme_ssd", "storage_nvme")

    if include_pricing:
        linux_prices = linux_on_demand_prices(item.get("pricing"))
        if linux_prices:
            output["linux_on_demand"] = linux_prices
        else:
            output["linux_on_demand"] = {}

    if include_benchmarks:
        copy_number(output, item, "coremark_iterations_second")
        copy_number(output, item, "ffmpeg_speed")
        copy_number(output, item, "ffmpeg_fps")

    return output


def linux_on_demand_prices(pricing: Any) -> dict[str, float]:
    if not isinstance(pricing, dict):
        return {}
    prices: dict[str, float] = {}
    for region, region_prices in pricing.items():
        if not isinstance(region, str) or not isinstance(region_prices, dict):
            continue
        linux = region_prices.get("linux")
        if not isinstance(linux, dict):
            continue
        price = parse_float(linux.get("ondemand"))
        if price is not None:
            prices[region] = price
    return dict(sorted(prices.items()))


def copy_string(
    output: dict[str, Any],
    source: dict[str, Any],
    source_key: str,
    output_key: str | None = None,
) -> None:
    value = source.get(source_key)
    if isinstance(value, str) and value:
        output[output_key or source_key] = value


def copy_bool(
    output: dict[str, Any],
    source: dict[str, Any],
    source_key: str,
    output_key: str | None = None,
) -> None:
    value = source.get(source_key)
    if isinstance(value, bool):
        output[output_key or source_key] = value


def copy_number(
    output: dict[str, Any],
    source: dict[str, Any],
    source_key: str,
    output_key: str | None = None,
) -> None:
    value = parse_float(source.get(source_key))
    if value is None:
        return
    if value.is_integer():
        output[output_key or source_key] = int(value)
    else:
        output[output_key or source_key] = value


def parse_float(value: Any) -> float | None:
    if isinstance(value, bool) or value is None:
        return None
    if isinstance(value, int | float):
        return float(value)
    if isinstance(value, str):
        try:
            return float(value)
        except ValueError:
            return None
    return None


if __name__ == "__main__":
    raise SystemExit(main())
