# The package set WeirdML v2's system prompt promises. torch comes with the base image.
FROM pytorch/pytorch:2.1.2-cuda12.1-cudnn8-runtime
RUN pip install --no-cache-dir numpy==1.26.4 scipy==1.11.4 pandas==2.1.4 scikit-learn==1.3.2 \
    torchvision==0.16.2 torchaudio==2.1.2 Pillow==10.1.0
# WeirdML runs on a 12 GB TITAN V. On a shared 80 GB H100, cap each process to the same 12 GB.
RUN printf 'import torch\nif torch.cuda.is_available():\n    torch.cuda.set_per_process_memory_fraction(12 / 80)\n' \
    > "$(python -c 'import site; print(site.getsitepackages()[0])')/sitecustomize.py"
WORKDIR /work
