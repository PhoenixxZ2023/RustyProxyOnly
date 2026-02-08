# RustyProxyOnly

**RustyProxyOnly** instala e gerencia o **RustyProxy**, um proxy/multiplexador TCP leve feito em **Rust**, com um **menu** para abrir múltiplas portas via **systemd**.

Ele foi pensado para cenários de VPS onde você quer expor várias portas e encaminhar o tráfego para serviços locais (ex.: **SSH** e **OpenVPN**) de forma simples.

## Como funciona

- O binário `RustyProxy` escuta em uma porta configurada.
- Ao receber uma conexão, ele decide para qual backend local encaminhar:
  - **SSH** → `127.0.0.1:22`
  - **OpenVPN** → `127.0.0.1:1194`
- O menu cria um serviço systemd por porta no formato:
  - `proxy@<PORTA>.service`
- O “status” exibido no banner é configurável por porta via arquivo:
  - `/etc/rustyproxy/proxy<PORTA>.env`

> **Observação:** este projeto não “instala SSH/OpenVPN”. Ele apenas encaminha conexões para serviços que já devem estar rodando na máquina.

---

## Requisitos

- Ubuntu: 18/20/22/24  
- Debian: 9/10/11/12  
- Acesso root (sudo)

Dependências instaladas automaticamente:
- `curl`, `git`, `build-essential`, `pkg-config`, `libssl-dev`, `ca-certificates`
- Rust toolchain via `rustup` (caso não exista `cargo`)

---

## Instalação

> Recomendo fixar uma tag/commit (instalação reproduzível). Se você ainda não usa releases/tags, pode instalar pela branch `main`.

### Instalar (branch main)

```bash
bash <(curl -fsSL https://raw.githubusercontent.com/PhoenixxZ2023/RustyProxyOnly/main/install.sh)

```bash
bash <(wget -qO- https://raw.githubusercontent.com/PhoenixxZ2023/RustyProxyOnly/refs/heads/main/install.sh)
```

