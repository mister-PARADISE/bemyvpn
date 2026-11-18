"""Отпечатки программ: по чему узнать, что работает у цели и какой версии.

Источники — всё, что сервер говорит сам: приветствие сервиса на порту (SSH, FTP, почта, MySQL…),
заголовки HTTP, имена cookie, страница (generator, title, подписи в подвале), адреса скриптов
и стилей (jquery-3.4.1.min.js, bootstrap@4.0.0), служебные адреса с версией (/status.php,
/api/health, /readme.html…). Каждая находка — продукт, версия, где и по какому признаку.

У продукта есть имена в NVD (vendor:product в CPE) — по ним модуль «Уязвимости» ищет CVE.
Пустой список — CVE не ищем (сервис-конструктор или протокол, а не программа).
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field

# --- продукты ------------------------------------------------------------------------


@dataclass(frozen=True)
class Product:
    id: str
    title: str
    category: str
    cpes: tuple[str, ...] = ()  # vendor:product в NVD; несколько — если продукт переименовывали
    target_sw: str = "*"  # «для какой платформы» в CPE: например, расширения, которые есть только для wordpress
    wp: str = ""  # plugin | theme — уязвимости ищем по названию в базе WordPress, а не в NVD

    @property
    def checkable(self) -> bool:
        """Есть ли где искать уязвимости этой программы."""
        return bool(self.cpes or self.wp)


_P = Product
PRODUCTS: dict[str, Product] = {p.id: p for p in (
    # веб-серверы и прокси
    _P("nginx", "nginx", "веб-сервер", ("f5:nginx", "nginx:nginx")),
    _P("apache", "Apache HTTP Server", "веб-сервер", ("apache:http_server",)),
    _P("iis", "Microsoft IIS", "веб-сервер", ("microsoft:internet_information_services",)),
    _P("lighttpd", "lighttpd", "веб-сервер", ("lighttpd:lighttpd",)),
    _P("litespeed", "LiteSpeed", "веб-сервер", ("litespeedtech:litespeed_web_server",)),
    _P("openlitespeed", "OpenLiteSpeed", "веб-сервер", ("litespeedtech:openlitespeed",)),
    _P("caddy", "Caddy", "веб-сервер", ("caddyserver:caddy",)),
    _P("openresty", "OpenResty", "веб-сервер", ("openresty:openresty",)),
    _P("tengine", "Tengine", "веб-сервер", ("alibaba:tengine",)),
    _P("tomcat", "Apache Tomcat", "сервер приложений", ("apache:tomcat",)),
    _P("jetty", "Eclipse Jetty", "сервер приложений", ("eclipse:jetty", "mortbay:jetty")),
    _P("weblogic", "Oracle WebLogic", "сервер приложений", ("oracle:weblogic_server",)),
    _P("squid", "Squid", "прокси", ("squid-cache:squid",)),
    _P("varnish", "Varnish", "прокси", ("varnish-cache:varnish",)),
    _P("haproxy", "HAProxy", "прокси", ("haproxy:haproxy",)),
    _P("envoy", "Envoy", "прокси", ("envoyproxy:envoy",)),
    _P("traefik", "Traefik", "прокси", ("traefik:traefik",)),
    _P("kong", "Kong Gateway", "прокси", ("konghq:kong_gateway",)),
    _P("trafficserver", "Apache Traffic Server", "прокси", ("apache:traffic_server",)),
    _P("minio", "MinIO", "хранилище", ("minio:minio",)),
    # языки, среды, фреймворки
    _P("php", "PHP", "язык", ("php:php",)),
    _P("python", "Python", "язык", ("python:python",)),
    _P("perl", "Perl", "язык", ("perl:perl",)),
    _P("nodejs", "Node.js", "среда", ("nodejs:node.js",)),
    _P("openssl", "OpenSSL", "библиотека", ("openssl:openssl",)),
    _P("mod_ssl", "mod_ssl", "библиотека", ("modssl:mod_ssl",)),
    _P("express", "Express", "фреймворк", ("openjsf:express",)),
    _P("nextjs", "Next.js", "фреймворк", ("vercel:next.js",)),
    _P("aspnet", "ASP.NET", "фреймворк", ("microsoft:asp.net",)),
    _P("werkzeug", "Werkzeug", "фреймворк", ("palletsprojects:werkzeug",)),
    _P("gunicorn", "Gunicorn", "сервер приложений", ("gunicorn:gunicorn",)),
    _P("uvicorn", "Uvicorn", "сервер приложений", ("encode:uvicorn",)),
    _P("django", "Django", "фреймворк", ("djangoproject:django",)),
    _P("laravel", "Laravel", "фреймворк", ("laravel:framework",)),
    _P("rails", "Ruby on Rails", "фреймворк", ("rubyonrails:rails",)),
    # CMS и магазины
    _P("wordpress", "WordPress", "CMS", ("wordpress:wordpress",)),
    _P("drupal", "Drupal", "CMS", ("drupal:drupal",)),
    _P("joomla", "Joomla!", "CMS", ("joomla:joomla\\!",)),
    _P("bitrix", "1С-Битрикс", "CMS", ("bitrix24:bitrix_site_manager", "bitrix24:bitrix24")),
    _P("magento", "Magento / Adobe Commerce", "CMS", ("magento:magento", "adobe:magento", "adobe:commerce")),
    _P("prestashop", "PrestaShop", "CMS", ("prestashop:prestashop",)),
    _P("opencart", "OpenCart", "CMS", ("opencart:opencart",)),
    _P("moodle", "Moodle", "CMS", ("moodle:moodle",)),
    _P("mediawiki", "MediaWiki", "CMS", ("mediawiki:mediawiki",)),
    _P("ghost", "Ghost", "CMS", ("ghost:ghost",)),
    _P("typo3", "TYPO3", "CMS", ("typo3:typo3",)),
    _P("matomo", "Matomo", "аналитика", ("matomo:matomo",)),
    _P("tilda", "Tilda", "конструктор сайтов"),
    _P("wix", "Wix", "конструктор сайтов"),
    _P("shopify", "Shopify", "конструктор сайтов"),
    # панели и инструменты
    _P("phpmyadmin", "phpMyAdmin", "панель", ("phpmyadmin:phpmyadmin",)),
    _P("adminer", "Adminer", "панель", ("adminer:adminer",)),
    _P("roundcube", "Roundcube", "веб-почта", ("roundcube:webmail",)),
    _P("grafana", "Grafana", "панель", ("grafana:grafana",)),
    _P("kibana", "Kibana", "панель", ("elastic:kibana",)),
    _P("jenkins", "Jenkins", "CI/CD", ("jenkins:jenkins",)),
    _P("gitlab", "GitLab", "CI/CD", ("gitlab:gitlab",)),
    _P("gitea", "Gitea", "CI/CD", ("gitea:gitea",)),
    _P("gogs", "Gogs", "CI/CD", ("gogs:gogs",)),
    _P("teamcity", "TeamCity", "CI/CD", ("jetbrains:teamcity",)),
    _P("nextcloud", "Nextcloud", "облако", ("nextcloud:nextcloud_server",)),
    _P("owncloud", "ownCloud", "облако", ("owncloud:owncloud",)),
    _P("zabbix", "Zabbix", "мониторинг", ("zabbix:zabbix",)),
    _P("prometheus", "Prometheus", "мониторинг", ("prometheus:prometheus",)),
    _P("vault", "HashiCorp Vault", "инфраструктура", ("hashicorp:vault",)),
    _P("consul", "HashiCorp Consul", "инфраструктура", ("hashicorp:consul",)),
    _P("portainer", "Portainer", "панель", ("portainer:portainer",)),
    _P("rabbitmq", "RabbitMQ", "очереди", ("vmware:rabbitmq", "pivotal_software:rabbitmq", "broadcom:rabbitmq_server")),
    _P("webmin", "Webmin", "панель", ("webmin:webmin",)),
    _P("cpanel", "cPanel", "панель", ("cpanel:cpanel",)),
    _P("plesk", "Plesk", "панель", ("plesk:plesk",)),
    _P("ispmanager", "ISPmanager", "панель"),
    _P("jira", "Jira", "трекер", ("atlassian:jira", "atlassian:jira_server", "atlassian:jira_data_center")),
    _P("confluence", "Confluence", "вики", ("atlassian:confluence", "atlassian:confluence_server",
                                          "atlassian:confluence_data_center")),
    _P("zimbra", "Zimbra", "почта", ("zimbra:collaboration", "synacor:zimbra_collaboration_suite")),
    _P("exchange", "Microsoft Exchange", "почта", ("microsoft:exchange_server",)),
    _P("sharepoint", "Microsoft SharePoint", "портал", ("microsoft:sharepoint_server",)),
    # базы и сервисы
    _P("mysql", "MySQL", "база данных", ("oracle:mysql", "mysql:mysql", "oracle:mysql_server")),
    _P("mariadb", "MariaDB", "база данных", ("mariadb:mariadb",)),
    _P("redis", "Redis", "база данных", ("redis:redis", "redislabs:redis")),
    _P("memcached", "Memcached", "кеш", ("memcached:memcached",)),
    _P("elasticsearch", "Elasticsearch", "база данных", ("elastic:elasticsearch", "elasticsearch:elasticsearch")),
    _P("docker", "Docker Engine", "контейнеры", ("docker:docker", "docker:engine", "mobyproject:moby")),
    _P("kubernetes", "Kubernetes", "контейнеры", ("kubernetes:kubernetes",)),
    # SSH, почта, FTP
    _P("openssh", "OpenSSH", "SSH", ("openbsd:openssh",)),
    _P("dropbear", "Dropbear SSH", "SSH", ("dropbear_ssh_project:dropbear_ssh",)),
    _P("libssh", "libssh", "SSH", ("libssh:libssh",)),
    _P("exim", "Exim", "почта", ("exim:exim",)),
    _P("postfix", "Postfix", "почта", ("postfix:postfix",)),
    _P("sendmail", "Sendmail", "почта", ("sendmail:sendmail",)),
    _P("opensmtpd", "OpenSMTPD", "почта", ("openbsd:opensmtpd",)),
    _P("dovecot", "Dovecot", "почта", ("dovecot:dovecot",)),
    _P("proftpd", "ProFTPD", "FTP", ("proftpd:proftpd",)),
    _P("vsftpd", "vsftpd", "FTP", ("beasts:vsftpd", "vsftpd_project:vsftpd")),
    _P("pureftpd", "Pure-FTPd", "FTP", ("pureftpd:pure-ftpd",)),
    _P("filezilla", "FileZilla Server", "FTP", ("filezilla-project:filezilla_server",)),
    _P("vnc", "VNC", "удалённый доступ"),
    # библиотеки в браузере
    _P("jquery", "jQuery", "библиотека JS", ("jquery:jquery",)),
    _P("jquery_ui", "jQuery UI", "библиотека JS", ("jqueryui:jquery_ui",)),
    _P("bootstrap", "Bootstrap", "библиотека JS", ("getbootstrap:bootstrap",)),
    _P("angularjs", "AngularJS", "библиотека JS", ("angularjs:angular.js",)),
    _P("vue", "Vue.js", "библиотека JS"),  # в NVD у Vue нет CPE — сверять не с чем
    _P("react", "React", "библиотека JS", ("facebook:react",)),
    _P("lodash", "Lodash", "библиотека JS", ("lodash:lodash",)),
    _P("moment", "Moment.js", "библиотека JS", ("momentjs:moment",)),
    _P("handlebars", "Handlebars", "библиотека JS", ("handlebarsjs:handlebars",)),
    _P("underscore", "Underscore.js", "библиотека JS", ("underscorejs:underscore",)),
    _P("dompurify", "DOMPurify", "библиотека JS", ("cure53:dompurify",)),
    _P("tinymce", "TinyMCE", "библиотека JS", ("tiny:tinymce", "tinymce:tinymce")),
    _P("ckeditor", "CKEditor", "библиотека JS", ("ckeditor:ckeditor",)),
    _P("prototype", "Prototype", "библиотека JS", ("prototypejs:prototype",)),
    _P("pdfjs", "PDF.js", "библиотека JS"),  # то же
)}


def product(pid: str) -> Product | None:
    """Продукт по id; плагины и темы WordPress (wp-plugin:slug) — на лету."""
    if pid in PRODUCTS:
        return PRODUCTS[pid]
    kind, _, slug = pid.partition(":")
    if kind in ("wp-plugin", "wp-theme") and slug:
        what = "плагин" if kind == "wp-plugin" else "тема"
        return Product(pid, f"WordPress {what} {slug}", f"WordPress: {what}", wp=kind[3:])
    return None


# --- находка -------------------------------------------------------------------------

@dataclass
class Hit:
    product: str
    version: str = ""
    evidence: str = ""
    distro: str = ""  # Ubuntu, Debian… — сборка дистрибутива: исправления могут быть перенесены без смены версии
    package: str = ""  # номер пакета дистрибутива, если виден (3ubuntu13, 0ubuntu0.22.04.1, 4+deb12u2)
    weight: int = 2  # насколько верить версии: файлы вроде readme.html остаются от старой установки — 1
    extra: dict = field(default_factory=dict)


_V = r"(?P<v>\d+(?:\.\d+)+[a-z]?\d*)"  # 1.24.0, 9.6, 1.0.2k, 9.6p1


def _rx(pattern: str) -> re.Pattern[str]:
    return re.compile(pattern.replace("{V}", _V), re.I)


# заголовки: (продукт, заголовок, шаблон)
HEADERS: list[tuple[str, str, re.Pattern[str]]] = [(p, h, _rx(x)) for p, h, x in (
    ("openresty", "server", r"openresty(?:/{V})?"),
    ("tengine", "server", r"Tengine(?:/{V})?"),
    ("nginx", "server", r"\bnginx(?:/{V})?"),
    ("apache", "server", r"\bApache(?![-\s]?(?:Coyote|Tomcat))(?:/{V})?"),
    ("tomcat", "server", r"Apache-Coyote"),
    ("openssl", "server", r"OpenSSL/{V}"),
    ("php", "server", r"PHP/{V}"),
    ("mod_ssl", "server", r"mod_ssl/{V}"),
    ("perl", "server", r"Perl/v?{V}"),
    ("python", "server", r"Python/{V}"),
    ("iis", "server", r"Microsoft-IIS(?:/{V})?"),
    ("lighttpd", "server", r"lighttpd(?:/{V})?"),
    ("openlitespeed", "server", r"OpenLiteSpeed(?:/{V})?"),
    ("litespeed", "server", r"^LiteSpeed(?:/{V})?"),
    ("caddy", "server", r"^Caddy\b"),
    ("jetty", "server", r"Jetty\((?P<v>\d+\.\d+\.\d+)"),
    ("werkzeug", "server", r"Werkzeug(?:/{V})?"),
    ("gunicorn", "server", r"gunicorn(?:/{V})?"),
    ("uvicorn", "server", r"\buvicorn\b"),
    ("squid", "server", r"squid(?:/{V})?"),
    ("envoy", "server", r"\benvoy\b"),
    ("kong", "server", r"kong(?:/{V})?"),
    ("minio", "server", r"\bMinIO\b"),
    ("webmin", "server", r"MiniServ(?:/{V})?"),
    ("trafficserver", "server", r"ATS(?:/{V})"),
    ("weblogic", "server", r"WebLogic(?: Server)?\s*{V}"),
    ("php", "x-powered-by", r"PHP(?:/{V})?"),
    ("aspnet", "x-powered-by", r"ASP\.NET"),
    ("express", "x-powered-by", r"^Express$"),
    ("nextjs", "x-powered-by", r"Next\.js(?:\s+{V})?"),
    ("plesk", "x-powered-by", r"Plesk"),
    ("aspnet", "x-aspnet-version", r"(?P<v>\d+\.\d+\.\d+)"),
    ("drupal", "x-generator", r"Drupal(?:\s+(?P<v>\d+))?"),
    ("joomla", "x-generator", r"Joomla"),
    ("drupal", "x-drupal-cache", r"."),
    ("drupal", "x-drupal-dynamic-cache", r"."),
    ("jenkins", "x-jenkins", r"(?P<v>\d+\.\d+(?:\.\d+)?)"),
    ("bitrix", "x-powered-cms", r"Bitrix"),
    ("exchange", "x-owa-version", r"(?P<v>\d+\.\d+\.\d+(?:\.\d+)?)"),
    ("kibana", "kbn-version", r"(?P<v>\d+\.\d+\.\d+)"),
    ("varnish", "via", r"varnish(?:\s*\(Varnish/{V}\))?"),
    ("varnish", "x-varnish", r"."),
    ("litespeed", "x-litespeed-cache", r"."),
    ("magento", "x-magento-cache-debug", r"."),
    ("magento", "x-magento-tags", r"."),
    ("confluence", "x-confluence-request-time", r"."),
    ("jira", "x-arequestid", r"."),
    ("gitlab", "x-gitlab-meta", r"."),
    ("sharepoint", "microsoftsharepointteamservices", r"(?P<v>\d+\.\d+\.\d+\.\d+)"),
    ("grafana", "x-grafana-version", r"(?P<v>\d+\.\d+\.\d+)"),
)]

# имена cookie
COOKIES: list[tuple[str, re.Pattern[str]]] = [(p, re.compile(x, re.I)) for p, x in (
    ("php", r"^PHPSESSID$"), ("aspnet", r"^ASP\.NET_SessionId$"), ("laravel", r"^laravel_session$"),
    ("django", r"^(?:csrftoken|django_language)$"), ("gitlab", r"^_gitlab_session$"),
    ("bitrix", r"^BITRIX_SM_"), ("wordpress", r"^(?:wordpress_|wp-settings-)"), ("grafana", r"^grafana_session$"),
    ("roundcube", r"^roundcube_sessid$"), ("phpmyadmin", r"^(?:phpMyAdmin|pma_lang)$"), ("prestashop", r"^PrestaShop-"),
    ("opencart", r"^OCSESSID$"), ("moodle", r"^MoodleSession"), ("nextcloud", r"^nc_"), ("jira", r"^atlassian\.xsrf\.token$"),
    ("tomcat", r"^JSESSIONID$"), ("rails", r"^_[a-z0-9_]+_session$"), ("magento", r"^(?:frontend|mage-)"),
    ("jenkins", r"^JSESSIONID\.[0-9a-f]+$"), ("zabbix", r"^zbx_session"), ("gitea", r"^i_like_gitea$"),
    ("gogs", r"^i_like_gogs$"), ("matomo", r"^MATOMO_SESSID$"),
)]

# приветствия сервисов на портах
BANNERS: list[tuple[str, re.Pattern[str]]] = [(p, _rx(x)) for p, x in (
    ("openssh", r"^SSH-[\d.]+-OpenSSH_(?:for_Windows_)?(?P<v>\d+\.\d+(?:p\d+)?)"),
    ("dropbear", r"^SSH-[\d.]+-dropbear_(?P<v>\d{4}\.\d+)"),
    ("libssh", r"^SSH-[\d.]+-libssh[_-](?P<v>\d+\.\d+\.\d+)"),
    ("proftpd", r"ProFTPD (?P<v>\d+\.\d+\.\d+[a-z]?)"),
    ("vsftpd", r"\(vsFTPd (?P<v>\d+\.\d+\.\d+)\)"),
    ("pureftpd", r"Pure-FTPd"),
    ("filezilla", r"FileZilla Server(?: version)? (?P<v>\d+\.\d+\.\d+)"),
    ("iis", r"Microsoft FTP Service"),
    ("exim", r"\bExim (?P<v>\d+\.\d+(?:\.\d+)?)"),
    ("postfix", r"ESMTP Postfix"),
    ("sendmail", r"Sendmail (?P<v>\d+\.\d+\.\d+)"),
    ("opensmtpd", r"ESMTP OpenSMTPD"),
    ("exchange", r"Microsoft ESMTP MAIL Service"),
    ("dovecot", r"\bDovecot\b"),
    ("mariadb", r"(?:5\.5\.5-)?(?P<v>\d+\.\d+\.\d+)-MariaDB"),
    ("mysql", r"^MySQL (?P<v>\d+\.\d+\.\d+)(?!.*MariaDB)"),
    ("redis", r"redis_version:(?P<v>\d+\.\d+\.\d+)"),
    ("redis", r"^-(?:NOAUTH|ERR operation not permitted|DENIED Redis)"),
    ("memcached", r"^VERSION (?P<v>\d+\.\d+\.\d+)"),
    ("vnc", r"^RFB \d{3}\.\d{3}"),
)]

# meta generator: текст из content=
GENERATORS: list[tuple[str, re.Pattern[str]]] = [(p, _rx(x)) for p, x in (
    ("wordpress", r"^WordPress(?:\s+{V})?"), ("joomla", r"Joomla!?"), ("drupal", r"Drupal(?:\s+(?P<v>\d+))?"),
    ("mediawiki", r"MediaWiki\s+{V}"), ("ghost", r"^Ghost(?:\s+{V})?"), ("typo3", r"TYPO3"),
    ("prestashop", r"PrestaShop"), ("bitrix", r"Bitrix"), ("tilda", r"Tilda"), ("wix", r"Wix\.com"),
    ("moodle", r"Moodle"), ("opencart", r"OpenCart"), ("magento", r"Magento"),
)]

# заголовок страницы
TITLES: list[tuple[str, re.Pattern[str]]] = [(p, _rx(x)) for p, x in (
    ("phpmyadmin", r"phpMyAdmin"), ("adminer", r"Adminer"), ("grafana", r"^Grafana$"), ("jenkins", r"\[Jenkins\]"),
    ("gitlab", r"GitLab"), ("gitea", r"\bGitea\b"), ("gogs", r"\bGogs\b"), ("nextcloud", r"Nextcloud"),
    ("owncloud", r"ownCloud"), ("zabbix", r"Zabbix"), ("roundcube", r"Roundcube"), ("portainer", r"Portainer"),
    ("kibana", r"^(?:Kibana|Elastic)$"), ("rabbitmq", r"RabbitMQ Management"), ("prometheus", r"Prometheus Time Series"),
    ("traefik", r"Traefik"), ("consul", r"Consul by HashiCorp"), ("vault", r"^Vault$"), ("minio", r"MinIO"),
    ("webmin", r"Webmin"), ("cpanel", r"cPanel"), ("plesk", r"Plesk"), ("ispmanager", r"ISPmanager"),
    ("moodle", r"Moodle"), ("zimbra", r"Zimbra"), ("exchange", r"Outlook(?: Web App)?$"), ("jira", r"\bJira\b|JIRA"),
    ("confluence", r"Confluence"), ("teamcity", r"TeamCity"), ("tomcat", r"Apache Tomcat(?:/{V})?"),
    ("nginx", r"Welcome to nginx"), ("apache", r"Apache2 (?:Ubuntu|Debian) Default Page|Test Page for the Apache"),
    ("iis", r"IIS Windows Server|Internet Information Services"), ("openresty", r"Welcome to OpenResty"),
)]

# признаки в тексте страницы (в том числе на странице ошибки 404)
BODY: list[tuple[str, re.Pattern[str]]] = [(p, _rx(x)) for p, x in (
    ("nginx", r"<center>nginx/{V}</center>"),
    ("openresty", r"<center>openresty/{V}</center>"),
    ("apache", r"<address>Apache/{V}"),
    ("tomcat", r"Apache Tomcat/(?P<v>\d+\.\d+\.\d+)"),
    ("lighttpd", r"lighttpd/{V}"),
    ("jetty", r"Powered by Jetty:// (?P<v>\d+\.\d+\.\d+)"),
    ("iis", r"Microsoft-IIS/(?P<v>\d+\.\d+)"),
    ("werkzeug", r"Werkzeug Debugger"),
    ("django", r"Using the URLconf defined in|csrfmiddlewaretoken"),
    ("wordpress", r"/wp-(?:content|includes)/"),
    ("wordpress", r"/wp-includes/js/wp-emoji-release\.min\.js\?ver=(?P<v>\d+\.\d+(?:\.\d+)?)"),
    ("wordpress", r"/wp-includes/css/dist/block-library/style\.min\.css\?ver=(?P<v>\d+\.\d+(?:\.\d+)?)"),
    ("joomla", r"/media/(?:system|jui)/js/|Joomla!"),
    ("drupal", r"Drupal\.settings|drupal-settings-json|data-drupal-|/sites/default/files/"),
    ("bitrix", r"/bitrix/(?:js|templates|cache|components)/|BX\.setCSSList|BX\.message"),
    ("tilda", r"tildacdn\.com|tilda\.ws"),
    ("wix", r"static\.wixstatic\.com|wix-warmup-data"),
    ("shopify", r"cdn\.shopify\.com"),
    ("magento", r"Mage\.Cookies|/static/version\d+/frontend/|mage/cookies"),
    ("prestashop", r"var prestashop\s*=|/modules/ps_"),
    ("opencart", r"catalog/view/theme/|route=common/home"),
    ("moodle", r"moodle-core|M\.cfg\s*="),
    ("gitea", r"Powered by Gitea(?:\s+Version)?:?\s*(?P<v>\d+\.\d+\.\d+)?"),
    ("gogs", r"Gogs\s+Version:\s*(?P<v>\d+\.\d+\.\d+)"),
    ("grafana", r"grafanaBootData|\"version\":\"(?P<v>\d+\.\d+\.\d+)\",\"commit\""),
    ("jira", r"<meta name=\"application-name\" content=\"JIRA\""),
    ("confluence", r"confluence-base-url|ajs-confluence"),
    ("teamcity", r"TeamCity (?:Professional |Enterprise )?(?P<v>20\d\d\.\d+(?:\.\d+)?)"),
    ("zabbix", r"Zabbix (?P<v>\d+\.\d+\.\d+)"),
    ("plesk", r"Plesk (?:Obsidian |Onyx )?(?P<v>\d+\.\d+\.\d+)"),
    ("adminer", r"<span class=\"version\">(?P<v>\d+\.\d+\.\d+)</span>"),
    ("exchange", r"/owa/auth/(?P<v>\d+\.\d+\.\d+(?:\.\d+)?)/"),
    ("roundcube", r"\"rcversion\":(?P<rc>\d{5})"),
    ("nextcloud", r"data-requesttoken|oc_config\b"),
    ("cpanel", r"cPanel_magic_revision"),
)]

# адреса скриптов и стилей: библиотеки с версией в имени файла или в пути CDN
SCRIPTS: list[tuple[str, re.Pattern[str]]] = [(p, _rx(x)) for p, x in (
    ("jquery", r"jquery[.-](?P<v>\d+\.\d+\.\d+)(?:\.slim)?(?:\.min)?\.js"),
    ("jquery", r"/jquery/(?P<v>\d+\.\d+\.\d+)/jquery"),
    ("jquery", r"jquery@(?P<v>\d+\.\d+\.\d+)"),
    ("jquery", r"/jquery(?:\.min)?\.js\?ver=(?P<v>\d+\.\d+\.\d+)"),
    ("jquery_ui", r"jquery-ui[.-](?P<v>\d+\.\d+\.\d+)"),
    ("jquery_ui", r"/jqueryui/(?P<v>\d+\.\d+\.\d+)/"),
    ("jquery_ui", r"jquery-ui@(?P<v>\d+\.\d+\.\d+)"),
    ("jquery_ui", r"/jquery/ui/[a-z-]+(?:\.min)?\.js\?ver=(?P<v>\d+\.\d+\.\d+)"),
    ("bootstrap", r"bootstrap[.-](?P<v>\d+\.\d+\.\d+)(?:\.bundle)?(?:\.min)?\.(?:js|css)"),
    ("bootstrap", r"/(?:twitter-)?bootstrap/(?P<v>\d+\.\d+\.\d+)/"),
    ("bootstrap", r"bootstrap@(?P<v>\d+\.\d+\.\d+)"),
    ("bootstrap", r"/bootstrap(?:\.bundle)?(?:\.min)?\.(?:js|css)\?ver=(?P<v>\d+\.\d+\.\d+)"),
    ("angularjs", r"angular[.-](?P<v>1\.\d+\.\d+)(?:\.min)?\.js"),
    ("angularjs", r"/angular\.js/(?P<v>1\.\d+\.\d+)/"),
    ("angularjs", r"angular@(?P<v>1\.\d+\.\d+)"),
    ("vue", r"vue@(?P<v>[23]\.\d+\.\d+)"),
    ("vue", r"/vue/(?P<v>[23]\.\d+\.\d+)/"),
    ("vue", r"vue[.-](?P<v>[23]\.\d+\.\d+)(?:\.min)?\.js"),
    ("react", r"react(?:-dom)?@(?P<v>\d+\.\d+\.\d+)"),
    ("react", r"/react/(?P<v>\d+\.\d+\.\d+)/"),
    ("lodash", r"lodash@(?P<v>\d+\.\d+\.\d+)"),
    ("lodash", r"/lodash\.js/(?P<v>\d+\.\d+\.\d+)/"),
    ("lodash", r"lodash[.-](?P<v>4\.\d+\.\d+)(?:\.min)?\.js"),
    ("lodash", r"/lodash(?:\.min)?\.js\?ver=(?P<v>4\.\d+\.\d+)"),
    ("moment", r"moment@(?P<v>2\.\d+\.\d+)"),
    ("moment", r"/moment\.js/(?P<v>2\.\d+\.\d+)/"),
    ("moment", r"moment[.-](?P<v>2\.\d+\.\d+)(?:\.min)?\.js"),
    ("moment", r"/moment(?:\.min)?\.js\?ver=(?P<v>2\.\d+\.\d+)"),
    ("handlebars", r"handlebars[@.-](?P<v>\d+\.\d+\.\d+)"),
    ("handlebars", r"/handlebars\.js/(?P<v>\d+\.\d+\.\d+)/"),
    ("underscore", r"underscore[@.-](?P<v>1\.\d+\.\d+)"),
    ("underscore", r"/underscore\.js/(?P<v>1\.\d+\.\d+)/"),
    ("underscore", r"/underscore(?:\.min)?\.js\?ver=(?P<v>1\.\d+\.\d+)"),
    ("dompurify", r"dompurify[@/](?P<v>\d+\.\d+\.\d+)"),
    ("dompurify", r"purify[.-](?P<v>\d+\.\d+\.\d+)(?:\.min)?\.js"),
    ("tinymce", r"tinymce[/@](?P<v>\d+\.\d+\.\d+)"),
    ("ckeditor", r"ckeditor[/@](?P<v>[45]\.\d+\.\d+)"),
    ("prototype", r"prototype[.-](?P<v>1\.\d+\.\d+(?:\.\d+)?)(?:\.min)?\.js"),
    ("pdfjs", r"pdf\.js/(?P<v>\d+\.\d+\.\d+)/"),
    ("pdfjs", r"pdfjs-dist@(?P<v>\d+\.\d+\.\d+)"),
)]

# библиотеки, чью версию можно прочитать из шапки самого файла, если в адресе её нет
FILE_BANNERS: list[tuple[str, re.Pattern[str], re.Pattern[str]]] = [
    (p, re.compile(name, re.I), _rx(banner)) for p, name, banner in (
        ("jquery", r"/jquery(?:\.min)?\.js(?:\?|$)", r"jQuery (?:JavaScript Library )?v(?P<v>\d+\.\d+\.\d+)"),
        ("jquery_ui", r"/jquery-ui(?:\.min)?\.js(?:\?|$)", r"jQuery UI - v(?P<v>\d+\.\d+\.\d+)"),
        ("bootstrap", r"/bootstrap(?:\.bundle)?(?:\.min)?\.js(?:\?|$)", r"Bootstrap v(?P<v>\d+\.\d+\.\d+)"),
        ("angularjs", r"/angular(?:\.min)?\.js(?:\?|$)", r"AngularJS v(?P<v>1\.\d+\.\d+)"),
        ("lodash", r"/lodash(?:\.min)?\.js(?:\?|$)", r"lodash (?:lodash\.com/license \| )?.*?(?P<v>4\.\d+\.\d+)"),
        ("moment", r"/moment(?:\.min)?\.js(?:\?|$)", r"(?:version|moment)\s*:?\s*(?P<v>2\.\d+\.\d+)"),
    )]

# служебные адреса, где программы называют версию; (путь, продукт, признак, версия)
JSON_PATHS: list[tuple[str, str, re.Pattern[str], re.Pattern[str]]] = [
    (path, p, re.compile(mark, re.I | re.S), _rx(ver)) for path, p, mark, ver in (
        ("/api/health", "grafana", r'"database"\s*:', r'"version"\s*:\s*"(?P<v>\d+\.\d+\.\d+)'),
        ("/status.php", "nextcloud", r'"productname"\s*:\s*"Nextcloud"', r'"versionstring"\s*:\s*"(?P<v>\d+\.\d+\.\d+)'),
        ("/status.php", "owncloud", r'"productname"\s*:\s*"ownCloud"', r'"versionstring"\s*:\s*"(?P<v>\d+\.\d+\.\d+)'),
        ("/api/status", "kibana", r'"kibana|"name"\s*:\s*"kibana', r'"number"\s*:\s*"(?P<v>\d+\.\d+\.\d+)'),
        ("/api/status", "portainer", r'"InstanceID"', r'"Version"\s*:\s*"(?P<v>\d+\.\d+\.\d+)'),
        ("/api/v1/status/buildinfo", "prometheus", r'"goVersion"', r'"version"\s*:\s*"(?P<v>\d+\.\d+\.\d+)'),
        ("/v1/sys/health", "vault", r'"sealed"\s*:', r'"version"\s*:\s*"(?P<v>\d+\.\d+\.\d+)'),
        ("/version", "docker", r'"ApiVersion"', r'"Version"\s*:\s*"(?P<v>\d+\.\d+\.\d+)'),
        ("/version", "kubernetes", r'"gitVersion"', r'"gitVersion"\s*:\s*"v(?P<v>\d+\.\d+\.\d+)'),
        ("/api/version", "traefik", r'"Codename"', r'"Version"\s*:\s*"(?P<v>\d+\.\d+\.\d+)'),
        ("/api/v1/version", "gitea", r'^\s*\{\s*"version"', r'"version"\s*:\s*"(?P<v>\d+\.\d+\.\d+)'),
        ("/", "elasticsearch", r"You Know, for Search", r'"number"\s*:\s*"(?P<v>\d+\.\d+\.\d+)'),
        ("/readme.html", "wordpress", r"WordPress", r"Version\s+(?P<v>\d+\.\d+(?:\.\d+)?)"),
        ("/wp-links-opml.php", "wordpress", r"generator=", r'generator="WordPress/(?P<v>\d+\.\d+(?:\.\d+)?)"'),
        ("/feed/", "wordpress", r"<generator>", r"wordpress\.org/\?v=(?P<v>\d+\.\d+(?:\.\d+)?)"),
        ("/CHANGELOG.txt", "drupal", r"^\s*Drupal 7\.", r"Drupal (?P<v>7\.\d+)"),
        ("/core/CHANGELOG.txt", "drupal", r"Drupal \d+\.\d+", r"Drupal (?P<v>\d+\.\d+\.\d+)"),
        ("/administrator/manifests/files/joomla.xml", "joomla", r"<extension", r"<version>(?P<v>\d+\.\d+\.\d+)</version>"),
        ("/language/en-GB/en-GB.xml", "joomla", r"Joomla", r"<version>(?P<v>\d+\.\d+\.\d+)</version>"),
    )]

# что спрашивать у веб-сервера, кроме главной: адреса с версией и пара известных панелей
PROBE_PATHS = (
    "/robots.txt", "/readme.html", "/wp-links-opml.php", "/feed/", "/CHANGELOG.txt", "/core/CHANGELOG.txt",
    "/administrator/manifests/files/joomla.xml", "/api/health", "/status.php", "/api/status",
    "/api/v1/status/buildinfo", "/v1/sys/health", "/version", "/api/version", "/api/v1/version",
    "/login", "/user/login", "/owa/", "/phpmyadmin/", "/adminer.php",
)
ROBOTS: list[tuple[str, re.Pattern[str]]] = [(p, re.compile(x, re.I)) for p, x in (
    ("wordpress", r"Disallow:\s*/wp-admin"), ("bitrix", r"Disallow:\s*/bitrix/"),
    ("joomla", r"Disallow:\s*/administrator/"), ("drupal", r"Disallow:\s*/core/|Disallow:\s*/user/register"),
    ("magento", r"Disallow:\s*/checkout/"), ("opencart", r"route=checkout"),
)]

WP_PLUGIN = re.compile(r"/wp-content/plugins/(?P<slug>[a-z0-9][a-z0-9_-]{1,60})/", re.I)
WP_THEME = re.compile(r"/wp-content/themes/(?P<slug>[a-z0-9][a-z0-9_-]{1,60})/", re.I)
WP_ASSET_VER = r"/wp-content/{kind}/{slug}/[^\"'\s?]+\?ver=(?P<v>\d+(?:\.\d+)+)"
STABLE_TAG = re.compile(r"^\s*Stable tag:\s*(?P<v>\d+(?:\.\d+)+)", re.I | re.M)
THEME_VERSION = re.compile(r"^\s*Version:\s*(?P<v>\d+(?:\.\d+)+)", re.I | re.M)

_META_GENERATOR = re.compile(r"<meta[^>]+name=[\"']generator[\"'][^>]*content=[\"']([^\"']+)"
                             r"|<meta[^>]+content=[\"']([^\"']+)[\"'][^>]*name=[\"']generator", re.I)
_TITLE = re.compile(r"<title[^>]*>(.*?)</title>", re.I | re.S)
_URLS = re.compile(r"(?:src|href)\s*=\s*[\"']([^\"']+)[\"']", re.I)
_AJS_VERSION = re.compile(r"<meta name=\"ajs-version-number\" content=\"(?P<v>\d+\.\d+(?:\.\d+)?)\"", re.I)
_PMA_VERSION = re.compile(r"[?&]v=(?P<v>\d+\.\d+\.\d+)[\"'&]", re.I)


# --- дистрибутив ---------------------------------------------------------------------

_DISTROS = (  # (название, номер пакета, просто упоминание)
    ("Ubuntu", re.compile(r"\d*ubuntu\d[\w.~+]*", re.I), re.compile(r"\bUbuntu\b", re.I)),
    ("Debian", re.compile(r"\d*\+?deb\d+u\d+", re.I), re.compile(r"\bDebian\b", re.I)),
    ("RHEL/CentOS", re.compile(r"(?<=\.)el\d+(?:_\d+)?\b", re.I), re.compile(r"Red Hat|CentOS|AlmaLinux|Rocky", re.I)),
    ("Alpine", None, re.compile(r"\bAlpine\b", re.I)),
    ("FreeBSD", None, re.compile(r"\bFreeBSD\b", re.I)),
    ("Windows", None, re.compile(r"\bWin(?:32|64|dows)\b|for_Windows", re.I)),
)


def distro_of(text: str) -> tuple[str, str]:
    """Сборка дистрибутива: («Ubuntu», «3ubuntu13»). Такие сборки часто получают исправления без смены версии."""
    for name, pkg, word in _DISTROS:
        m = pkg.search(text or "") if pkg is not None else None
        if m:
            return name, m.group(0)
        if word.search(text or ""):
            return name, ""
    return "", ""


# --- распознавание -------------------------------------------------------------------

def _hit(pid: str, m: re.Match[str] | None, evidence: str, context: str = "") -> Hit:
    version = ""
    if m is not None:
        gd = m.groupdict()
        version = gd.get("v") or ""
        if gd.get("rc"):  # Roundcube: 10603 → 1.6.3
            n = int(gd["rc"])
            version = f"{n // 10000}.{(n // 100) % 100}.{n % 100}"
    distro, package = distro_of(context or evidence)
    return Hit(pid, version.rstrip("."), evidence[:160], distro, package)


def from_headers(headers: dict[str, str], cookies: list[dict]) -> list[Hit]:
    hits = []
    for pid, name, rx in HEADERS:
        value = headers.get(name)
        if value is None:
            continue
        m = rx.search(value)
        if m:
            hits.append(_hit(pid, m, f"{name}: {value}"))
    names = [c.get("name", "") for c in cookies or []]
    for pid, rx in COOKIES:
        for n in names:
            if rx.search(n):
                hits.append(Hit(pid, evidence=f"cookie {n}"))
                break
    return hits


def from_banner(text: str) -> list[Hit]:
    hits = []
    for pid, rx in BANNERS:
        m = rx.search(text or "")
        if m:
            hits.append(_hit(pid, m, text.strip()[:160]))
            if pid == "mariadb":
                break  # «5.5.5-10.6.12-MariaDB» — это не MySQL 5.5.5
    return hits


def from_body(html: str, base: str = "") -> list[Hit]:
    """Страница: generator, title, признаки в тексте, библиотеки в адресах скриптов, Jira/Confluence."""
    hits: list[Hit] = []
    for m in _META_GENERATOR.finditer(html or ""):
        text = (m.group(1) or m.group(2) or "").strip()
        for pid, rx in GENERATORS:
            g = rx.search(text)
            if g:
                hits.append(_hit(pid, g, f"generator: {text}"))
    t = _TITLE.search(html or "")
    if t:
        title = " ".join(t.group(1).split())[:120]
        for pid, rx in TITLES:
            g = rx.search(title)
            if g:
                hits.append(_hit(pid, g, f"title: {title}"))
    for pid, rx in BODY:
        g = rx.search(html or "")
        if g:  # рядом с версией часто сказано и про дистрибутив: «Apache/2.4.41 (Ubuntu)»
            hits.append(_hit(pid, g, f"на странице{(' ' + base) if base else ''}: {g.group(0)[:80]}",
                             context=html[g.start():g.end() + 40]))
    if "phpmyadmin" in (html or "").lower():
        g = _PMA_VERSION.search(html)
        if g:
            hits.append(_hit("phpmyadmin", g, f"phpMyAdmin, ресурсы ?v={g.group('v')}"))
    a = _AJS_VERSION.search(html or "")
    if a:
        low = html.lower()
        pid = "confluence" if "confluence" in low else "jira" if "jira" in low else ""
        if pid:
            hits.append(_hit(pid, a, f"ajs-version-number {a.group('v')}"))
    for url in urls_of(html):
        for pid, rx in SCRIPTS:
            g = rx.search(url)
            if g:
                hits.append(_hit(pid, g, f"подключён {url[:120]}"))
                break
    return hits


def urls_of(html: str) -> list[str]:
    return list(dict.fromkeys(m.group(1) for m in _URLS.finditer(html or "")))


STATIC_PATHS = {"/readme.html", "/CHANGELOG.txt", "/core/CHANGELOG.txt", "/language/en-GB/en-GB.xml"}


def from_path(path: str, body: str) -> list[Hit]:
    """Ответ служебного адреса: JSON с версией, readme, changelog, robots.txt."""
    hits = []
    for p, pid, mark, ver in JSON_PATHS:
        if p != path or not mark.search(body or ""):
            continue
        m = ver.search(body)
        h = _hit(pid, m, f"{path}: {_short_json(body)}" if m else f"{path} отвечает")
        h.weight = 1 if path in STATIC_PATHS else 2
        hits.append(h)
    if path == "/robots.txt":
        for pid, rx in ROBOTS:
            if rx.search(body or ""):
                hits.append(Hit(pid, evidence=f"robots.txt: {rx.pattern.split(':')[-1].strip()[:40]}"))
    return hits


def _short_json(text: str) -> str:
    try:
        data = json.loads(text)
        return json.dumps(data, ensure_ascii=False)[:120]
    except (ValueError, TypeError):
        return " ".join((text or "").split())[:120]


def wp_assets(html: str) -> tuple[dict[str, str], dict[str, str]]:
    """Плагины и темы WordPress со страницы: {slug: версия из ?ver= или ''}."""
    plugins: dict[str, str] = {}
    themes: dict[str, str] = {}
    for kind, rx, out in (("plugins", WP_PLUGIN, plugins), ("themes", WP_THEME, themes)):
        for m in rx.finditer(html or ""):
            slug = m.group("slug").lower()
            if slug in out and out[slug]:
                continue
            v = re.search(WP_ASSET_VER.format(kind=kind, slug=re.escape(slug)), html, re.I)
            out[slug] = v.group("v") if v else out.get(slug, "")
    return plugins, themes
