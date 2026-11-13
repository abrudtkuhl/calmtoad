# Calm Toad

Calm Toad checks a website for common problems like missing page titles, broken links, and images without descriptions.

[![Watch Calm Toad crawl pmprompt.com](assets/pmprompt-crawl.png)](https://cdn2.48web.com/assets/frogvtoad%202026-11-10%20at%2013.40.39.mp4)

[Watch the side-by-side demo](https://cdn2.48web.com/assets/frogvtoad%202026-11-10%20at%2013.40.39.mp4) · [Calm Toad website source on GitHub](https://github.com/abrudtkuhl/calmtoad-website)

## Install Calm Toad

The easiest way to install Calm Toad is to ask an AI assistant that can use your computer. Copy and paste this:

```text
Please install Calm Toad on this computer for me. Use the official download for my computer:

- Windows: https://github.com/abrudtkuhl/calmtoad/releases/latest/download/Calm-Toad-Windows.zip
- Mac with an Apple chip: https://github.com/abrudtkuhl/calmtoad/releases/latest/download/Calm-Toad-Mac-Apple-Silicon.zip
- Mac with an Intel processor: https://github.com/abrudtkuhl/calmtoad/releases/latest/download/Calm-Toad-Mac-Intel.zip
- Linux: https://github.com/abrudtkuhl/calmtoad/releases/latest/download/Calm-Toad-Linux.tar.gz

Figure out which link matches this computer. If you are unsure, ask me before downloading. Install Calm Toad in the usual place for apps and open it to make sure it works. Ask me before using an administrator password or changing security settings. If something gets in the way, explain it in plain language. When you're done, tell me where Calm Toad is and confirm that it opened.
```

### Install it yourself

**Windows:** [Download Calm Toad for Windows](https://github.com/abrudtkuhl/calmtoad/releases/latest/download/Calm-Toad-Windows.zip). In Downloads, right-click the file, choose **Extract All**, open the new folder, and double-click **Calm Toad**.

**Mac:** Choose the [Apple chip download](https://github.com/abrudtkuhl/calmtoad/releases/latest/download/Calm-Toad-Mac-Apple-Silicon.zip) for an M-series Mac, or the [Intel download](https://github.com/abrudtkuhl/calmtoad/releases/latest/download/Calm-Toad-Mac-Intel.zip). If you are unsure, open the Apple menu and choose **About This Mac**. Open the download, open the **Calm Toad** folder it creates, then drag **Calm Toad.app** into Applications.

**Linux:** [Download Calm Toad for Linux](https://github.com/abrudtkuhl/calmtoad/releases/latest/download/Calm-Toad-Linux.tar.gz). Open the download, choose **Extract**, then open the new folder and double-click **Calm Toad**. If Linux asks whether to run or display it, choose **Run**. If it does not open, use the instructions above to ask an AI assistant to set it up.

The Mac app is not signed by Apple yet, so macOS may show a security warning the first time you open it. If you choose to continue, follow [Apple's instructions](https://support.apple.com/en-us/102445).

### Updates

Calm Toad checks for a new version when it opens and every 24 hours after that.
When one is available, a strip offers the download; nothing installs until you
choose **Restart and install**, so an active crawl is never interrupted.
Downloads are verified against a signed manifest before they are used. If Calm
Toad lives somewhere it cannot write to (or macOS is running it from a temporary
location), it explains that instead of installing.

Versions installed before this updater existed need one manual upgrade. After
that, updates are handled inside the app. You can also choose **Check for
Updates…** from the app menu at any time.

To try it, open Calm Toad, enter `https://pmprompt.com/`, and press **Crawl**.

Leave **Use sitemaps** on and Calm Toad also reads the site's sitemaps while it crawls. It uses the `Sitemap:` declarations in robots.txt; if there are none, it tries `/sitemap.xml` and then `/sitemap_index.xml`. A guessed file that is missing is not reported as a problem, but a declared sitemap that fails is. URLs from a sitemap join the crawl as entry points and share its page limit, so a large sitemap does not quietly crawl forever.

The **Sitemaps** tab shows the URLs the sitemaps list and the files that were read. Switch between **URLs** and **Files** above the table. Each URL says whether it was checked, blocked by robots.txt, skipped, or left by the page limit, and checked URLs carry the same response, canonical, and noindex findings as the other tabs. **Issues only** narrows either view to rows that need attention.

The **Internal** tab answers a narrower question than “is this indexed?”: **what did this crawl observe that may prevent or discourage indexing, and where did the signal come from?** The **INDEXABILITY** column uses one of eight labels: Fetch failed, HTTP error, Redirect, Noindex, Review canonical, Canonical elsewhere, Not assessed, or No block found. A noindex rule is a hard signal, while a canonical pointing elsewhere, a redirect, or incomplete evidence is an advisory. Not assessed means the crawl could not finish the HTML check, for example a successful PDF, a 304, or a body cut off at the size limit. A plain successful non-HTML response is context, not an issue: it stays out of Issues only and sitemap issue counts, while the drawer, CSV, and Sitemaps row still explain why it was not assessed. No block found means the supported checks saw no noindex or canonical concern; it is not a claim that Google has indexed the URL or chosen its canonical.

Select a row to open the page details drawer. It lists every finding and its source, the raw robots meta and `X-Robots-Tag` declarations, each canonical declaration with its resolved target and whether that target was fetched or left unchecked, and the read state. Robots rules are applied only when they are generic or scoped to Googlebot; other agent rules stay visible but are marked as scoped. When more directives or canonicals were present than the evidence cap retains, the drawer, CSV, and Sitemaps evidence say so, and an applicable noindex or nofollow in the omitted tail still decides the label. The Internal **Issues only** filter uses the same assessment, so advisories and confirmed issues are included; informational context and clean rows are not.

Use **Export CSV** above the results to save the active tab or Sitemaps view. The file includes the columns and rows currently shown, so the active sort and **Issues only** filter are applied. On Internal, the export adds **REASONS** and **EVIDENCE** columns so the row label can be traced back to the exact declarations.

## Building or improving Calm Toad

Developers: see [CONTRIBUTING.md](CONTRIBUTING.md) for setup, checks, pull requests, and releases.

For automated installs, the downloads are also available through [GitHub Packages](https://github.com/abrudtkuhl/calmtoad/pkgs/container/calmtoad).
