FROM python:3.14-slim

ENV PYTHONPATH=/app:/app/src
ENV UV_INSTALL_DIR=/usr/local/bin
ENV NBE_NODE_API=http
ENV NBE_NODE_MANAGER=noop
ENV NBE_NODE_API_HOST=host.docker.internal
ENV NBE_NODE_API_PORT=8080

RUN apt-get update && apt-get install -y curl git && rm -rf /var/lib/apt/lists/*

RUN curl -LsSf https://astral.sh/uv/install.sh | sh

RUN git clone https://github.com/logos-blockchain/logos-blockchain-block-explorer-template.git /app

WORKDIR /app

RUN git checkout dda9e4e714d4c8e6b67c1d723ddcfce3d3773cbf

RUN uv pip compile pyproject.toml -o requirements.txt && uv pip install --system -r requirements.txt

EXPOSE 8000

CMD ["python", "/app/src/main.py"]
