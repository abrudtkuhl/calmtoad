# Calm Toad

A desktop website crawler for SEO audits. Crawl a site, then review its URLs, page titles, descriptions, headings, response codes, canonicals, images, and internal links.

![Calm Toad showing a crawl of pmprompt.com](assets/pmprompt-crawl.png)

## Download and install

Go to [the latest Calm Toad download](https://github.com/abrudtkuhl/calmtoad/releases/latest) and choose the download for your computer:

| Your computer | Choose this download | Then |
| --- | --- | --- |
| Windows PC | File ending in `windows-x86_64.zip` | In Downloads, right-click the file, choose **Extract All**, open the new folder, and double-click `calmtoad.exe`. |
| Mac with an Apple chip (M1, M2, M3, or later) | File ending in `macos-aarch64.zip` | Open the download and drag `Calm Toad.app` into Applications. Open Calm Toad from Applications. |
| Mac with an Intel processor | File ending in `macos-x86_64.zip` | Open the download and drag `Calm Toad.app` into Applications. Open Calm Toad from Applications. |
| Linux PC | File ending in `linux-x86_64.tar.gz` | Open the download and choose **Extract**. Open the folder it creates and launch Calm Toad. |

To check which Mac you have, open the Apple menu and choose **About This Mac**. If it says **Chip**, choose the Apple chip download; if it says **Processor**, choose Intel. If Calm Toad does not open on Linux, use the agent instructions below to have it set up for you.

The Mac app is not signed by Apple yet, so macOS may ask you to approve it the first time you open it. If you choose to continue, follow [Apple's instructions](https://support.apple.com/en-us/102445). These downloads are also available through [GitHub Packages](https://github.com/abrudtkuhl/calmtoad/pkgs/container/calmtoad).

### Have an agent install it

Copy this instruction into an agent that can use your computer:

```text
Please install Calm Toad for me.

1. Get the latest version from the official download page: https://github.com/abrudtkuhl/calmtoad/releases/latest.
2. Choose the download for this computer. On a Mac, check **About This Mac** to see whether it has an Apple chip or an Intel processor.
3. Install Calm Toad in the normal Apps folder for my account and open it to make sure it works.
4. Ask me before entering an administrator password, installing system software, or changing security settings. If the Mac shows a security warning, stop and explain it to me. Do not turn off Mac security.
5. Tell me where Calm Toad is installed and whether it opened successfully.
```

## Build and run

Requires a Rust toolchain that supports edition 2024.

```sh
cargo run
```

Enter `https://pmprompt.com/` to try a real crawl. Calm Toad follows links on that site, reads `robots.txt`, respects `X-Robots-Tag: noindex`, and streams results into the table. It does not render JavaScript.

## Contribute

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, checks, pull requests, and releases.
