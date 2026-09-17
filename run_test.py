import subprocess
try:
    subprocess.check_output(
        ["cargo", "test", "vdp1_polygon_and_distorted", "--", "--nocapture"],
        stderr=subprocess.STDOUT
    )
except subprocess.CalledProcessError as e:
    print(e.output.decode())

