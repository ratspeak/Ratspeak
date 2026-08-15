/* Ratspeak legal and support documents bundled for offline access.
 * Keep this snapshot aligned with the canonical copies at ratspeak.org.
 */
(function() {
    'use strict';

    window.RS = window.RS || {};

    var URLS = Object.freeze({
        privacy: 'https://ratspeak.org/privacy.html',
        terms: 'https://ratspeak.org/terms.html',
        guidelines: 'https://ratspeak.org/community-guidelines.html',
        support: 'https://ratspeak.org/support.html'
    });

    var DOCUMENTS = Object.freeze({
        privacy: {
            title: 'Privacy Policy',
            eyebrow: 'Privacy',
            headline: 'Your device is the center of Ratspeak.',
            lede: 'Ratspeak does not require an account, phone number, analytics profile, or advertising identifier. This policy explains the narrower cases where information leaves your device.',
            meta: 'Effective August 15, 2026 · Ratspeak LLC',
            url: URLS.privacy,
            content: `
                <section>
                    <h2>Privacy at a glance</h2>
                    <div class="rs-legal-principles">
                        <div><strong>No Ratspeak account</strong><span>Your identity and conversation database are created and stored on your device.</span></div>
                        <div><strong>No app tracking</strong><span>The Ratspeak app does not include advertising SDKs, cross-app tracking, or app analytics.</span></div>
                        <div><strong>Direct messages are encrypted</strong><span>Direct LXMF messages are end-to-end encrypted for their intended recipient.</span></div>
                        <div><strong>Channels are different</strong><span>A channel hub relays and can read channel messages. Independent hubs have their own operators and policies.</span></div>
                    </div>
                    <div class="rs-legal-notice"><strong>Important network boundary</strong><p>Ratspeak is a client for a decentralized network. Relays, interfaces, propagation nodes, and channel hubs may be run by independent third parties. Ratspeak LLC does not own, operate, moderate, or control them unless a service is clearly identified as an official Ratspeak service.</p></div>
                </section>
                <section><h2>Who and what this policy covers</h2><p>This policy applies to the Ratspeak application, ratspeak.org, and services explicitly operated by Ratspeak LLC. It does not govern independently operated Reticulum infrastructure, channel hubs, websites, or other third-party services. Those operators may process network data under their own terms and policies.</p><p>When this policy says “Ratspeak,” “we,” or “us,” it means Ratspeak LLC.</p></section>
                <section><h2>Information stored on your device</h2><p>Depending on the features you use, Ratspeak stores information locally such as:</p><ul><li>Reticulum and LXMF identities, keys, addresses, and display names;</li><li>contacts, trust decisions, blocked peers, paths, and peer observations;</li><li>messages, attachments, voice messages, channel history, game state, and call history;</li><li>interface, propagation-node, theme, accessibility, privacy, and channel settings; and</li><li>diagnostic and activity information shown inside the app.</li></ul><p>This local information is not automatically uploaded to Ratspeak LLC. Anyone with access to your unlocked device, device backup, exported identity, or recovery material may be able to access some of it. Protect your device and recovery material.</p></section>
                <section><h2>Direct messages, media, and calls</h2><h3>Direct communications</h3><p>Ratspeak uses Reticulum and LXMF to route direct communications. Message content is encrypted for the destination identity, but network participants may observe limited routing and timing information needed to deliver traffic. Recipients receive their own copy and may retain, copy, or share it. Ratspeak cannot recall content from a recipient or their backups.</p><h3>Calls and voice messages</h3><p>Audio is processed when you choose to place or answer a call or record a voice message. Live call traffic is transported over the network; voice messages are delivered like other message content. Ratspeak does not use microphone audio for advertising or analytics.</p></section>
                <section><h2>Propagation nodes and relays</h2><p>A propagation node can temporarily store encrypted LXMF payloads so a recipient can retrieve them later. Ratspeak LLC operates some propagation nodes listed in the app’s node directory. Those nodes may process encrypted payloads and the protocol metadata needed to queue, route, limit abuse, and expire traffic. Where an interface uses IP networking, the node can also process the connecting IP address and it may appear in operational logs. Independent propagation nodes are controlled by their own operators.</p><p>Ratspeak-operated propagation nodes are not intended to read the content of end-to-end encrypted direct messages or use network addresses to profile users. Queued traffic is removed as it is delivered, expires, or is removed under operational limits. Exact network copies may also exist on independently operated infrastructure outside our control.</p></section>
                <section><h2>Public and shared channels</h2><p><strong>Channel messages do not have the same privacy model as direct messages.</strong> A channel hub relays and can read messages submitted to its channels. Other channel participants may also retain or redistribute them.</p><p>Unless a hub is clearly labeled as an official Ratspeak hub, it is independently operated. Ratspeak LLC does not select its participants, review all content, guarantee its availability, or control its moderation and retention practices. Independent hubs may expose you to offensive, misleading, harmful, or illegal content. You can leave a hub, block participants locally, and report safety concerns from the app.</p><p>Ratspeak does not currently operate a public channel hub. If we begin doing so, official hubs will be clearly identified and their moderation rules will be published. A hub merely appearing in discovery does not make it an official Ratspeak service or an endorsement by Ratspeak.</p></section>
                <section><h2>Website, downloads, and support</h2><p>Ratspeak.org uses Vercel Web Analytics to measure aggregate website usage, including pages and routes visited, referrers, country, browser, operating system, and device type. This website measurement is separate from the Ratspeak app and is not linked to your Ratspeak identity, contacts, messages, or in-app activity.</p><p>Vercel and our website hosting and security providers may also process ordinary request information such as IP address, time, requested page, browser details, and security signals to deliver, measure, and protect the site. We do not use this information to build advertising profiles.</p><p>If you email mail@ratspeak.org, submit a safety report, or send beta feedback, we receive the information you choose to provide, your email address, and related correspondence. Channel reports are prepared for your review and are not sent automatically.</p></section>
                <section><h2>Device permissions</h2><p>Ratspeak requests platform permissions only when needed for a feature. Depending on your device, these can include local network access, Bluetooth, microphone, camera access for QR scanning, photos or files for attachments and exports, and notifications. You can change permissions in system settings, although the related feature may stop working.</p></section>
                <section><h2>Retention, deletion, and security</h2><p>Local app data remains until you delete it through Ratspeak, clear application data, or uninstall the app, subject to device backups. Deleting local data does not delete copies already delivered to other people or stored by independent infrastructure.</p><p>Support correspondence and safety reports are retained only as reasonably needed to respond, protect users, document action, and meet legal obligations. You may ask us to delete support information associated with your email, subject to safety, fraud-prevention, and legal retention needs. Official node data follows operational expiry and capacity limits.</p><p>We use technical and organizational safeguards appropriate to the information we operate, but no device, radio link, network, or storage system is perfectly secure.</p></section>
                <section><h2>Your choices and controls</h2><ul><li>Use Ratspeak without creating a centralized Ratspeak account.</li><li>Choose your interfaces and propagation node, or operate compatible infrastructure.</li><li>Decline optional permissions or public-channel access.</li><li>Block peers, leave channels, and prepare a report for safety review.</li><li>Delete local conversations, channel history, contacts, identities, or app data using the available controls.</li><li>Contact us to access or delete information you intentionally sent to Ratspeak support.</li></ul><h3>Age eligibility</h3><p>Ratspeak is intended for adults age 18 and older and is not designed for children. We do not knowingly collect personal information from children through official Ratspeak services. If you believe a child provided information to an official service, contact us so we can investigate and delete it where appropriate.</p></section>
                <section><h2>Questions or requests</h2><p>We may update this policy as Ratspeak and its official services change. Material changes will receive a new effective date and, when appropriate, an in-app notice.</p><div class="rs-legal-contact"><div><strong>Privacy, safety, and legal contact</strong><span>Tell us what you need and which Ratspeak service is involved.</span></div><button type="button" data-legal-email="Ratspeak privacy request">Email Ratspeak</button></div></section>
            `
        },
        terms: {
            title: 'Terms of Use',
            eyebrow: 'Terms of use',
            headline: 'Use the network responsibly.',
            lede: 'These terms cover the Ratspeak app and services expressly operated by Ratspeak LLC. The wider Reticulum network is decentralized and includes infrastructure we do not control.',
            meta: 'Effective August 15, 2026 · Ratspeak LLC',
            url: URLS.terms,
            content: `
                <section><h2>Your agreement with Ratspeak</h2><p>By installing or using Ratspeak, using an official Ratspeak service, or accepting the public-channel notice, you agree to these Terms and the <button type="button" class="rs-legal-inline-link" data-legal-document="guidelines">Community Guidelines</button>. If you do not agree, do not use the affected feature or service.</p><div class="rs-legal-notice"><strong>Independent infrastructure is not a Ratspeak service</strong><p>A relay, interface, propagation node, or channel hub is not operated, endorsed, or moderated by Ratspeak LLC unless it is clearly identified as official.</p></div></section>
                <section><h2>Eligibility</h2><p>You must be at least 18 years old and legally able to agree to these Terms. Ratspeak is not designed or offered for use by children.</p></section>
                <section><h2>Beta and pre-release software</h2><p>Pre-release versions are provided for evaluation and may be incomplete, unstable, or incompatible with later versions. They may lose local data, use additional battery or network resources, or contain security and reliability defects. Back up important identity material and do not rely on beta software for emergency, life-safety, or legally required communications.</p><p>Beta feedback you intentionally send may be used to diagnose and improve Ratspeak. Do not include secrets or third-party personal information unless it is necessary for the report.</p></section>
                <section><h2>A decentralized, independently operated network</h2><p>Ratspeak is a client that can communicate over Reticulum. Much of that network is built and operated by other people. We do not promise that a path, peer, node, radio interface, propagation node, or hub is safe, lawful, available, accurate, or operated by the person it claims to be.</p><p>You are responsible for choosing infrastructure, complying with radio, spectrum, export, privacy, and communications laws that apply to you, and understanding the capabilities and limits of your equipment.</p></section>
                <section><h2>Public and shared channels</h2><p>Public channels may contain content from strangers and may be hosted on independent infrastructure. The hub can read messages submitted to it. Independent operators decide their own access, moderation, retention, and availability practices.</p><p>Discovery or display inside Ratspeak does not mean Ratspeak LLC operates, verifies, recommends, or endorses a hub or its content. Future official Ratspeak hubs will be clearly labeled and governed by published rules. You can leave, block a participant locally, and report a safety concern, but we may be unable to remove content from an independent hub.</p></section>
                <section><h2>Acceptable use</h2><p>You may not use Ratspeak or an official Ratspeak service to:</p><ul><li>threaten violence, stalk, harass, promote hateful attacks, or encourage self-harm;</li><li>abuse, exploit, coerce, or endanger another person;</li><li>share intimate material without consent, expose private information, or steal credentials;</li><li>distribute malware, phishing, scams, spam, or traffic intended to disrupt a network or device;</li><li>impersonate another person or falsely present an independent service as an official Ratspeak service;</li><li>infringe intellectual-property or other legal rights;</li><li>evade access controls, abuse limits, moderation, or safety measures; or</li><li>violate applicable law or facilitate another person’s violation.</li></ul><p>The fuller behavioral standard is in the <button type="button" class="rs-legal-inline-link" data-legal-document="guidelines">Community Guidelines</button>.</p></section>
                <section><h2>Your content and responsibility</h2><p>You retain the rights you have in content you send. You grant recipients and the infrastructure you intentionally use the limited technical permission needed to receive, relay, queue, display, and store that content. You are responsible for your content, your backups, your exported identities, and the people or infrastructure with whom you share it.</p><p>Once content is delivered, other people or independent systems may retain it. Deleting your local copy or account-free identity does not recall their copies.</p></section>
                <section><h2>Reports, blocking, and enforcement</h2><p>Ratspeak provides local blocking and a way to prepare reports to mail@ratspeak.org. Nothing is sent without your review. We can investigate app conduct and official services, restrict access to official services, remove an official listing, or contact an appropriate operator when warranted.</p><p>Independent hub operators control their hubs. We cannot guarantee they will respond or remove content. Reports made in bad faith, intended to harass, or containing knowingly false information violate these Terms.</p></section>
                <section><h2>Changes and availability</h2><p>We may change, suspend, or discontinue a beta feature or official service; change operational limits; or require a newer app version when reasonably needed for safety, security, interoperability, or maintenance. The decentralized network may continue independently of Ratspeak LLC.</p></section>
                <section><h2>Open-source components</h2><p>Source code and third-party components are licensed under the licenses included with their respective repositories or distributions. Those licenses govern your rights to copy, modify, and redistribute that code. These Terms govern your use of official Ratspeak services and do not replace applicable open-source licenses.</p></section>
                <section><h2>Service limits</h2><p>To the extent permitted by law, Ratspeak and official beta services are provided “as is” and “as available,” without promises that delivery, routing, identity claims, moderation, discovery, or storage will be uninterrupted or error-free. Ratspeak is not an emergency service.</p><p>To the extent permitted by law, Ratspeak LLC is not responsible for content, conduct, infrastructure, outages, or data practices of independent network operators or users. Nothing in these Terms excludes rights or remedies that cannot lawfully be excluded.</p></section>
                <section><h2>Changes and contact</h2><p>We may update these Terms as the app and official services change. A material update will receive a new effective date and, where appropriate, a new in-app acceptance request.</p><div class="rs-legal-contact"><div><strong>Questions about these Terms</strong><span>Include the feature or official service involved.</span></div><button type="button" data-legal-email="Ratspeak terms">Email Ratspeak</button></div></section>
            `
        },
        guidelines: {
            title: 'Community Guidelines',
            eyebrow: 'Community guidelines',
            headline: 'Open networks still need human boundaries.',
            lede: 'These rules define acceptable conduct in Ratspeak and on official Ratspeak services, while explaining what Ratspeak can—and cannot—do on independently operated infrastructure.',
            meta: 'Effective August 15, 2026 · 18+',
            url: URLS.guidelines,
            content: `
                <section><h2>Where these guidelines apply</h2><p>These Guidelines apply to your use of the Ratspeak app and official Ratspeak services. They also describe the standard we use when evaluating reports and deciding whether infrastructure should appear in an official Ratspeak directory.</p><div class="rs-legal-notice"><strong>Not every hub is ours</strong><p>Independent channel hubs and other Reticulum infrastructure are run by their own operators. Ratspeak LLC cannot continuously monitor, moderate, or remove content from systems it does not control.</p></div><p>Ratspeak is intended for adults age 18 and older.</p></section>
                <section><h2>Treat people as people</h2><p>Communicate without coercion, targeted abuse, or deception. Respect another person’s privacy, boundaries, identity, and decision to disengage. A decentralized network does not make harmful conduct consequence-free.</p></section>
                <section><h2>Content and conduct we do not allow</h2><ul><li><strong>Violence and targeted harm:</strong> credible threats, coordination of violence, stalking, or encouragement of self-harm.</li><li><strong>Harassment and hateful conduct:</strong> sustained abuse, intimidation, or attacks directed at a person or protected group.</li><li><strong>Abuse and exploitation:</strong> coercion, predatory behavior, exploitation, or conduct that endangers another person. Content or conduct that abuses, exploits, or endangers a child is prohibited.</li><li><strong>Privacy and consent violations:</strong> doxxing, credential theft, intimate material shared without consent, or exposing information that creates a safety risk.</li><li><strong>Fraud and deception:</strong> impersonation, scams, phishing, or falsely presenting an independent service as official Ratspeak infrastructure.</li><li><strong>Technical and network abuse:</strong> malware, spam, denial of service, resource exhaustion, or attempts to bypass safety and access controls.</li><li><strong>Illegal material:</strong> content or conduct that violates applicable law or infringes another person’s rights.</li></ul></section>
                <section><h2>Be a good network participant</h2><p>Reticulum can operate over scarce, shared, and community-managed links. Do not flood interfaces, exhaust storage, abuse discovery, forge official Ratspeak status, or deliberately degrade service for others. Follow operator policies and the radio and communications rules that apply where you are.</p></section>
                <section><h2>Public channels and independent hubs</h2><p>A channel hub can read the channel messages it relays. Join only hubs you trust. A listing or discovery result is not an endorsement, and an independent hub may apply different rules or no meaningful moderation at all.</p><p>Official Ratspeak hubs, when available, will be clearly labeled and will follow these Guidelines. We may remove an independent hub from an official directory or warn users when credible reports show severe abuse, deception, or safety risk. That does not remove the underlying hub from the decentralized network.</p></section>
                <section><h2>Block, leave, and report</h2><ul><li><strong>Block:</strong> use the person view to stop displaying that participant’s channel activity locally. Network blackholing may also be available for known identities.</li><li><strong>Leave:</strong> disconnect from a channel or hub if it is unsafe, unwanted, or not what it claimed to be.</li><li><strong>Report:</strong> use the report action to prepare an email, review what will be shared, add context, and send it to mail@ratspeak.org.</li></ul><p>Preserve relevant identifiers, dates, hub and channel names, and a short description. Do not forward illegal imagery or expose more personal information than necessary. Reports are not sent automatically.</p></section>
                <section><h2>How Ratspeak responds</h2><p>For official services, Ratspeak may investigate, warn, restrict access, remove content or listings, preserve evidence where legally required, and refer credible imminent threats or illegal material to appropriate authorities. We prioritize imminent danger, exploitation, non-consensual intimate material, serious privacy violations, and severe or repeated targeted abuse.</p><p>For an independent hub, we may provide safety guidance, contact its operator when known, suppress it from an official directory, or act on other official Ratspeak services. We cannot promise removal from infrastructure we do not operate. We do not guarantee an individual outcome, but we will review good-faith reports as promptly as reasonably possible.</p></section>
                <section><h2>Immediate danger and support</h2><p>Ratspeak is not an emergency service and cannot locate a peer from a nickname alone. If someone is in immediate danger, contact the emergency service or a trusted local organization appropriate to your location.</p><div class="rs-legal-contact"><div><strong>Safety report or moderation question</strong><span>Include “Urgent” only for a credible, time-sensitive safety concern.</span></div><button type="button" data-legal-email="Ratspeak safety report">Email Ratspeak</button></div></section>
            `
        },
        support: {
            title: 'Support and Safety',
            eyebrow: 'Support and safety',
            headline: 'Start with the right kind of help.',
            lede: 'Technical support, beta feedback, privacy requests, and safety reports all reach the same small team—use a clear subject so the urgent things surface first.',
            meta: 'Official contact: mail@ratspeak.org',
            url: URLS.support,
            content: `
                <section><h2>Contact Ratspeak</h2><p>Email is the official support, privacy, legal, and safety contact. Ratspeak does not currently provide phone support.</p><div class="rs-legal-contact"><div><strong>mail@ratspeak.org</strong><span>Include your platform, Ratspeak version, and the feature involved when relevant.</span></div><button type="button" data-legal-email="Ratspeak support">Compose email</button></div></section>
                <section><h2>Technical support</h2><p>For a reproducible problem, include:</p><ul><li>Ratspeak version and whether it came from TestFlight, GitHub, or another package;</li><li>device model and operating-system version;</li><li>what you expected, what happened, and the exact sequence that triggers it;</li><li>whether the issue persists after reopening the app; and</li><li>screenshots or relevant activity entries with private keys, recovery words, and sensitive addresses removed.</li></ul><p>Never send an identity private key, recovery phrase, PIN, or unredacted secret. Public issue trackers are useful for non-sensitive software bugs; email is better for security or private network details.</p></section>
                <section><h2>TestFlight feedback</h2><p>You can use Apple’s TestFlight feedback interface for crashes and screenshots or email us directly. Beta software can change quickly, so include the build number shown in Ratspeak. If a build blocks access to your identity or messages, stop retrying destructive actions and contact us first.</p></section>
                <section><h2>Report a safety problem</h2><p>Use the in-app Report action when available. It prepares a message for you to review; nothing is sent automatically. You can also email us with:</p><ul><li>the hub and channel name and hub address;</li><li>the person’s visible name, identity hash, or LXMF address if available;</li><li>the approximate date and time;</li><li>a concise description of the conduct and why it creates a safety issue; and</li><li>whether the hub was clearly labeled as official Ratspeak infrastructure.</li></ul><p>Do not attach illegal, exploitative, or non-consensual material, malware, or more private information than necessary. Describe the issue and preserve relevant identifiers instead.</p></section>
                <section><h2>Independent hubs and nodes</h2><p>Most Reticulum infrastructure is not owned or controlled by Ratspeak LLC. We can review app behavior, official listings, and official services. We may contact an independent operator if one is known, but only that operator can moderate or delete content from its hub.</p><p>If an independent hub is unsafe, leave it and block the relevant participant. A hub appearing in discovery is not proof that Ratspeak operates or endorses it.</p></section>
                <section><h2>Privacy and deletion requests</h2><p>Most Ratspeak information lives on your device and can be removed with local controls or by clearing app data. For support correspondence or data intentionally sent to an official Ratspeak service, email us from the relevant address with “Privacy request” in the subject. We may need enough information to locate the correspondence and verify the request.</p><p>See the <button type="button" class="rs-legal-inline-link" data-legal-document="privacy">Privacy Policy</button> for what cannot be recalled from recipients or independent network infrastructure.</p></section>
                <section><h2>Not an emergency service</h2><div class="rs-legal-notice"><strong>Immediate danger</strong><p>Ratspeak cannot dispatch help, reliably identify a person from a network nickname, or guarantee delivery. Contact emergency services or a trusted local safety organization appropriate to your location.</p></div><p>For a credible, time-sensitive issue involving an official Ratspeak service, use the subject “Urgent safety report” and explain why it is urgent. Do not use that label for ordinary support requests.</p></section>
            `
        }
    });

    function showError(message) {
        if (typeof showToast === 'function') {
            showToast(message, 'toast-error', 5000);
        }
    }

    function openDocument(documentId) {
        var initial = DOCUMENTS[documentId];
        if (!initial || typeof _rsBuildSheet !== 'function') return false;

        var currentId = documentId;
        var built = _rsBuildSheet({
            title: initial.title,
            showTitle: false,
            ariaLabel: initial.title,
            nativeBackValue: true
        });
        built.sheet.classList.add('rs-legal-sheet');

        var article = document.createElement('article');
        article.className = 'rs-legal-document';
        built.body.appendChild(article);

        var online = document.createElement('button');
        online.type = 'button';
        online.className = 'nr-btn nr-btn-secondary rs-legal-online';
        online.textContent = 'View current version online';
        online.addEventListener('click', function() {
            var current = DOCUMENTS[currentId];
            Promise.resolve(RS.openExternalUrl(current.url)).then(function(opened) {
                if (opened === false) throw new Error('The online copy could not be opened.');
            }).catch(function(error) {
                showError((error && error.message) || 'The online copy could not be opened.');
            });
        });

        var done = document.createElement('button');
        done.type = 'button';
        done.className = 'nr-btn nr-btn-primary';
        done.textContent = 'Done';
        done.addEventListener('click', function() { built.dismiss(true); });
        built.footer.appendChild(online);
        built.footer.appendChild(done);

        function render(nextId) {
            var legalDocument = DOCUMENTS[nextId];
            if (!legalDocument) return;
            currentId = nextId;
            built.sheet.setAttribute('aria-label', legalDocument.title);
            article.innerHTML =
                '<header class="rs-legal-hero">' +
                    '<span class="rs-legal-eyebrow">' + legalDocument.eyebrow + '</span>' +
                    '<h1>' + legalDocument.headline + '</h1>' +
                    '<p>' + legalDocument.lede + '</p>' +
                    '<small>' + legalDocument.meta + ' · Available offline</small>' +
                '</header>' +
                '<div class="rs-legal-content">' + legalDocument.content + '</div>';
            built.body.scrollTop = 0;
            online.setAttribute('aria-label', 'View the current online ' + legalDocument.title);
        }

        article.addEventListener('click', function(event) {
            var documentLink = event.target.closest('[data-legal-document]');
            if (documentLink) {
                event.preventDefault();
                render(documentLink.getAttribute('data-legal-document'));
                return;
            }
            var emailLink = event.target.closest('[data-legal-email]');
            if (!emailLink) return;
            event.preventDefault();
            var subject = emailLink.getAttribute('data-legal-email') || 'Ratspeak support';
            Promise.resolve(RS.openSupportEmail(subject, '')).then(function(opened) {
                if (opened === false) throw new Error('No email app is available.');
            }).catch(function(error) {
                showError((error && error.message) || 'No email app is available.');
            });
        });

        built.overlay.addEventListener('click', function(event) {
            if (event.target === built.overlay) built.dismiss(true);
        });
        render(documentId);
        built.present();
        return true;
    }

    window.RS.legal = Object.freeze({
        version: '2026-08-15',
        urls: URLS,
        documents: DOCUMENTS,
        open: openDocument
    });
})();
