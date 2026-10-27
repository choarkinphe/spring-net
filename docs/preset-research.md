# AoNet 低频探测书签来源记录

资料整理日期：2026-10-26。正式收录 **55 个目标、7 个列表**。

> **重要：资料核对不等于当前可达，更不等于网站允许压测。**
> 这些是第三方官方 HTTPS 首页的低频探测书签，不是公共压力测试靶场、测速端点、API 清单或监控服务承诺。此次只进行了官方来源资料搜索和联盟目录阅读，没有启动 AoNet，没有执行目标连通性、状态码、延迟、吞吐、并发或压力测试。搜索索引和网页阅读服务的结果也不能证明使用者所在网络当前可访问。
>
> `probe_only: true` 是用途标记，不是技术限流或授权机制。`builtin: true` 仅表示内置列表。使用前须另行确认站点条款、robots 政策、所在网络及法律要求；只在允许范围内低频使用，不绕过验证码、WAF、地域或登录限制。遇到 403、429、验证码或拒绝访问应停止，不自动重试加压。

## 范围与数量

| 文件 | 列表 | 目标数 |
| --- | --- | ---: |
| `presets/cn-airlines.json` | 中国大陆航司 | 10 |
| `presets/asia-airlines.json` | 亚太航司，含港澳台及大洋洲 | 7 |
| `presets/global-airlines.json` | 欧美及中东航司 | 10 |
| `presets/cn-ota.json` | 中国在线旅行平台 | 4 |
| `presets/global-travel.json` | 国际 OTA、比价、住宿与旅行内容平台 | 11 |
| `presets/cn-web.json` | 中国常用网站 | 6 |
| `presets/global-web.json` | 全球常用网站 | 7 |
| **合计** | **7 个列表** | **55** |

采用地区与用途覆盖的编辑选样，不声称全球实时排名、市场份额排名、流量排名或完整行业覆盖。OTA、比价、住宿分享和点评平台不是同一业务类型，放入旅行列表仅为了检索方便。平台规模、联盟成员数量及搜索摘要中的宣传性排名不作为收录依据。

## 数据约定

- 每文件为单个 JSON 对象，顶层只有 `id`、`name`、`description`、`builtin`、`targets`。
- 每个目标只有 `id`、`name`、`url`、`region`、`source`、`checked_at`、`probe_only`。没有为研究记录添加模型外字段。
- 列表与目标的全部 ID 跨 7 文件全局唯一，使用小写 ASCII kebab-case slug。
- `url` 一律为官方 HTTPS 根首页，不含查询参数、片段、搜索路径、登录路径、预订接口、值机或订单管理接口。百度和 Google 只收录首页，不提交搜索请求。
- `source` 为官方资料 URL，可以是集团品牌页、区域首页或实际检索到的首页；来源页不是探测目标，可以包含语言参数或首页路径。
- `region` 使用国家/地区代码（如 CN、HK、TW、JP、US）或 `GLOBAL`。航司表示所属地区，旅行和全球网站的 `GLOBAL` 表示面向多地区用户；不表示服务器、IP、CDN 出口或数据存储所在地。分类是编辑用途，并不作政治地理判断。
- 官方区域首页的证据用于确认品牌与域名，`url` 统一取该域名根首页。例如 ANA、JAL、国泰、澳航、阿联酋、英航、美联航、美国航空和 Microsoft。未测试根首页重定向、最终页面、HTTP 状态或地区选择行为。
- KLM 检索链接带搜索引擎生成的 `srsltid` 跟踪参数，保存的首页及来源去除了该参数；不据此产生查询或预订 URL。
- 京东采用实际检索确认的 `https://m.jd.com/` 移动首页，显示名称明确标注；不将 `home.jd.com` 登录页冒充公共首页。
- `checked_at` 全部为 `2026-10-26`，仅表示本次已取得的资料证据在该日完成整理核对，不是网站可用性测试时间、官方页面更新时间或独立重新访问时间。下文明确区分用户提供的已核实研究与本轮新增搜索；未获充分资料的候选没有进入正式 JSON。

## 核对方法与证据边界

1. 复用任务中明确提供的已核实官方来源，共覆盖 **22 个目标**；不虚构又进行了一次独立访问。
2. 对其余 **33 个目标**进行批次搜索，先按航司、OTA 与常用网站分组，再针对混合搜索未覆盖的品牌使用官方域名限定检索。正式依据是实际返回的官方域名结果标题、链接及摘要；未声称逐个打开全部 55 个目标。
3. 首个 WebSearch 工具返回“无法进行实时搜索”和参考链接，因此其结果**不计作核验依据**。本轮新增证据来自后续实际返回结果的 Web Search Prime；其检索结果仍可能滞后，不能冒充实时网站状态。
4. 阅读了 [oneworld 成员目录](https://www.oneworld.com/members) 与 [Star Alliance 成员目录](https://www.staralliance.com/en/members) 作为背景资料。目录正文未完整给出所需外链，Star Alliance 成员区域还有动态加载限制，因此没有把成员名称直接推断成官网地址。新增航司最终采用各自官方域名的搜索证据。
5. 部分 WebFetch 请求因安全校验无法读取；未将失败调用写成成功核验。联盟目录后来由资料阅读工具获得正文，也不构成对航司目标的连通性测试。
6. 本地检查仅解析 JSON、核对字段、ID、数量、日期和 URL 形式，不发起网络请求。

## 复用的已核实来源：22 项

以下来源由本任务明确提供为已核实研究，本轮复用。品牌中文名用于显示，目标域名与品牌关系以该研究为依据。

| 官方来源 | 正式目标 | 数量 |
| --- | --- | ---: |
| [Trip.com Group 品牌页](https://group.trip.com/brands) | 携程 `www.ctrip.com`、去哪儿 `www.qunar.com`、Trip.com `www.trip.com`、Skyscanner `www.skyscanner.net` | 4 |
| [Booking Holdings](https://www.bookingholdings.com/) | Booking.com `www.booking.com`、Agoda `www.agoda.com`、Priceline `www.priceline.com`、KAYAK `www.kayak.com` | 4 |
| [Expedia Group](https://www.expediagroup.com/en-us/) | Expedia `www.expedia.com`、Hotels.com `www.hotels.com`、Vrbo `www.vrbo.com` | 3 |
| [中国国际航空](https://www.airchina.com.cn/) | 中国国际航空 | 1 |
| [中国东方航空](https://www.ceair.com/) | 中国东方航空 | 1 |
| [中国南方航空](https://www.csair.com/) | 中国南方航空 | 1 |
| [海南航空](https://www.hnair.com/) | 海南航空 | 1 |
| [厦门航空](https://www.xiamenair.com/) | 厦门航空 | 1 |
| [四川航空](https://www.sichuanair.com/) | 四川航空 | 1 |
| [深圳航空](https://global.shenzhenair.com/) | 深圳航空国际首页 | 1 |
| [春秋航空](https://www.ch.com/) | 春秋航空 | 1 |
| [新加坡航空](https://www.singaporeair.com/) | 新加坡航空 | 1 |
| [汉莎航空](https://www.lufthansa.com/) | 汉莎航空 | 1 |
| [卡塔尔航空](https://www.qatarairways.com/) | 卡塔尔航空 | 1 |

## 本轮实际检索的官方资料：33 项

下列为官方域名检索结果，不是第三方旅行代理、论坛、百科条目或社交账户推断。标题是检索结果的识别信息，不作标题中营销表述的背书。

| 目标 ID | 官方来源 | 检索结果识别信息 |
| --- | --- | --- |
| `juneyao-air` | [吉祥航空](https://www.juneyaoair.com/) | 吉祥航空；国内机票预订 |
| `shandong-airlines` | [山东航空](https://www.sda.cn/) | 山东航空官方网站；24 小时服务热线 |
| `cathay-pacific` | [国泰区域首页](https://www.cathaypacific.com/cx/en_US.html) | Book flights & elevate your life / Cathay US |
| `ana` | [ANA 区域首页](https://www.ana.co.jp/en/us) | Flights to Tokyo / ANA English；另有官方区域选择页结果 |
| `japan-airlines` | [JAL 区域首页](https://www.jal.co.jp/en-jp) | Book Domestic Flights；摘要明确 JAL Official Website |
| `korean-air` | [大韩航空首页](https://www.koreanair.com/?hl=en) | Korean Air / Book your flight ticket |
| `eva-air` | [长荣航空首页](https://www.evaair.com/) | Homepage / EVA Air |
| `qantas` | [澳航区域首页](https://www.qantas.com/en-us) | Fly with the Spirit of Australia / Qantas US |
| `emirates` | [阿联酋区域首页](https://www.emirates.com/us/english) | Emirates / Fly Better |
| `etihad-airways` | [阿提哈德首页](https://www.etihad.com/) | Etihad Airways / Book flights, manage and check-in online |
| `british-airways` | [英国航空区域首页](https://www.britishairways.com/travel/home/public/zh_cn) | 英国航空；直接在 ba.com 上预订 |
| `air-france` | [法国航空首页](https://www.airfrance.com/) | Air France；服务、品牌及官网导航摘要 |
| `klm` | [荷兰皇家航空首页](https://www.klm.com/) | KLM Royal Dutch Airlines / Book flights online |
| `united-airlines` | [美联航区域首页](https://www.united.com/zh-hant/tw) | United Airlines / Airline Tickets, Travel Deals and Flights |
| `delta-air-lines` | [达美航空首页](https://www.delta.com/) | Delta Air Lines / Flights & Plane Tickets |
| `american-airlines` | [美国航空首页来源](https://www.aa.com/homePage.do?locale=en_US) | American Airlines / Airline tickets and low fares at aa.com |
| `fliggy` | [飞猪首页](https://www.fliggy.com/) | 首页标题“飞猪”，摘要为空；同域官方旅行频道与规则中心结果补充品牌归属 |
| `tongcheng-travel` | [同程旅行首页](https://www.ly.com/) | 同程旅行；一站式旅游预订平台 |
| `airbnb` | [Airbnb 首页](https://www.airbnb.com/) | Airbnb / Vacation rentals, cabins, beach houses, & more |
| `tripadvisor` | [Tripadvisor 首页](https://www.tripadvisor.com/) | Tripadvisor / reviews & contributions for Hotels |
| `baidu` | [百度首页](https://www.baidu.com/) | 百度一下；搜索引擎首页，无搜索请求参数 |
| `tencent` | [腾讯首页](https://www.tencent.com/) | Tencent；另有腾讯中文首页结果 |
| `jd` | [京东移动首页](https://m.jd.com/) | 京东；超市、电器、服饰美妆等首页导航 |
| `alibaba-group` | [阿里巴巴集团首页](https://www.alibabagroup.com/) | Alibaba Group Official Website |
| `netease` | [网易首页](https://www.163.com/) | 手机网易网；网易产品与内容频道摘要 |
| `bilibili` | [哔哩哔哩首页](https://www.bilibili.com/) | 哔哩哔哩 / bilibili |
| `microsoft` | [Microsoft 区域首页](https://www.microsoft.com/en-us) | Microsoft / AI, Cloud, Productivity, Computing, Gaming & Apps |
| `apple` | [Apple 首页](https://www.apple.com/) | Apple；产品与服务首页摘要 |
| `github` | [GitHub 首页](https://github.com/) | GitHub / Change is constant. GitHub keeps you ahead |
| `wikipedia` | [Wikipedia 多语言首页](https://www.wikipedia.org/) | Wikipedia；志愿者编辑、Wikimedia Foundation 托管的百科 |
| `google` | [Google 首页](https://www.google.com/) | Google；另有 Google 官方帮助页说明首页地址 |
| `amazon` | [Amazon 首页](https://www.amazon.com/) | Amazon.com / Spend less. Smile more. |
| `cloudflare` | [Cloudflare 首页](https://www.cloudflare.com/) | Cloudflare；官方平台首页摘要 |

飞猪补充官方证据：[旅行目的地频道](https://www.fliggy.com/place)、[规则中心](https://rule.fliggy.com/)。这些只作为资料来源，没有作为探测 URL；没有因首页搜索摘要为空就声称读取到了其首页正文。

## 未正式纳入与原因

- **新西兰航空**：批次结果主要返回长荣航空与新西兰目的地信息，以及联盟成员名称；没有取得足够直接的该品牌官方首页证据。首个工具给出的参考域名不算实时检索，不凭记忆补入。
- **土耳其航空**：搜索未取得其官方首页直接结果；联盟成员名称不能代替官网 URL 证据。为满足数量而猜域名不合适，因此不纳入。
- **韩亚航空**：检索出现 ANA 官方合作变更信息，涉及 2026 年 10 月的合作结束；这不等于官网已关闭，也不能证明品牌首页资料，未继续追查，暂不纳入。
- **京东桌面根首页候选** `www.jd.com`：本轮相关结果以商品页、拍卖页及登录页居多，不能把它们当作桌面首页正文证据；正式收录的是已检索确认的官方移动首页，不声称桌面首页不可达。
- **JAL 的 `jal.com` 候选**：首个工具仅给出参考链接，后续实际官方结果确认的是 `jal.co.jp`，因此只收录后者，不重复计数。
- **搜索、登录、预订、值机与订单管理页面**：即使来自官方域名，也一律不作为 `url`。例如 `accounts.google.com`、`home.jd.com`、ANA Flight Search、美国航空 `booking/find-flights`、携程等业务接口均不属于本清单目标。
- **第三方代理、论坛、社交账户、百科里的品牌词条**：不作为品牌官网确认的唯一依据。Wikipedia 自身门户的官方结果可用于确认 Wikipedia，但 Wikipedia 中的航司或企业词条不代替这些品牌的官方来源。
- **更多未研究候选及同一品牌的区域镜像**：没有实际取得资料证据的不收录；不为了凑数引入，也不通过区域重复项制造覆盖数量。

## 业务连接资料目录（2026-10-27）

`presets/connections/` 是独立只读 schema，不属于上方 7 类 / 55 项公共首页书签。连接资料通过共享 bootstrap 供浏览器与桌面查看，不进入 HEAD 选择器、用户列表复制/导入导出或承载授权。读取详情不做 DNS、TCP、TLS、HEAD、API 或登录操作。

| 配置卡 | 官方证据 | 收录边界 |
| --- | --- | --- |
| Travelport Smartpoint Cloud | [实际入口](https://support.travelport.com/webhelp/mytravelport/Content/SPC.htm)、[端点要求](https://support.travelport.com/webhelp/smartpointcloud/Content/SPC/Endpoints.htm) | `https://sp.travelport.com`；完整23条放行规则（22个通配域及goprivate.wspan.com），端口均明确443；规则页未逐项明示协议，不用端口倒推协议 |
| Galileo / Smartpoint Desktop 12 | [登录说明](https://support.travelport.com/webhelp/smartpoint1g1v/Content/SignOn12/SignOn12_1G-SignOn.htm) | 证明产品登录涉及GTID/connection及profile，不是完整网络矩阵；Cloud配置不冒充Desktop配置 |
| Travelport JSON Air API v11 | [API地址](https://support.travelport.com/webhelp/JSONAPIs/Airv11/Content/Air11/General/AirEndpointsList11.htm)、[认证与迁移](https://support.travelport.com/webhelp/JSONAPIs/Airv11/Content/GeneralProject/Oauth.htm) | 当前.net生产/预生产与OAuth地址；API不代替Galileo客户端，不获取token、不调用业务接口 |
| Sabre Mosaic Agency Workspace | [产品](https://www.sabre.com/agency-mosaic/experience-suite/agency-workspace)、[官方支持](https://www.sabre.com/support-agencies) | formerly Sabre Red 360，浏览器/桌面并存；当前客户端完整主机/端口未知，需Hub中适用版本的Connectivity Guide |
| Amadeus Selling Platform Connect | [产品](https://amadeus.com/en/travel-sellers/products/selling-platform-connect)、[入口身份](https://www.sellingplatformconnect.amadeus.com) | 实际读取入口仅显示加载状态；官方搜索索引出现登录字段，但未登录或验证完整跳转链。HTTPS默认443为标准推导，不冒称防火墙文档明确给出 |
| TravelSky IBE+ | [官方产品](https://www.travelsky.cn/mainen/Business/Dis/UniversalInterface/2023/12/I1189867891038617600.html) | Internet Booking Engine Plus；官方索引发布日2014-06-23，URL中的2023/12不是版本。无公开生产主机/端口/协议，用户实际接入是否为同一产品待确认 |

核对日期表示本次资料整理，不表示端点实时可达或官方页面更新。研究使用官方网页、官方域名搜索索引及Context7文档结果；没有向业务接口发起测试、认证或订座请求。门户无法读取完整正文和资料缺项均保留为限制，而不是补造连接参数。

没有采用旧Travelport Booking Feed 10.1的`gdssl-atl.galileo.com:443`或特定专线1415作为当前Galileo通用配置，也没有采用Sabre区域旧版release notes或第三方转载清单。Travelport旧`.com` API/OAuth端点有官方迁移/弃用提示，新资料采用当前`.net`地址。Sabre REST/SOAP文档虽可查到API端点，但本批不以其填补客户端参数。

通配域仅展示放行规则，不能解析`*.domain`、枚举子域或用虚构的子域进行测试。共享云/CDN域不是供应商专属服务；不能因为它出现在依赖清单就认为整个云域属于GDS，或整体阻止用户为自己的受控云端点登记授权。

旧资源包如果没有连接资料子目录，会明确显示目录未提供；损坏的资料文件报加载错误，不静默生成看似有效的数据。此目录没有运行授权含义，认证/订座/出票地址即使公开也不自动请求。

## 使用与维护提示

这些 JSON 不含调度、频率、并发和重试配置，也不实施任何探测代码。后续若集成调度，应另行设计默认不自动运行、逐目标低频、全局并发约束、退避与停止策略，并先满足目标方要求；这不是当前文件提供的能力或授权。

资料更新时应重新核对官方来源与域名，记录真实核对日期；不能批量刷新日期而不查证。重定向、语言选择、移动适配、CDN、地域限制和反自动化策略均可能变化，需与来源研究和业务可用性分别记录。本次不对这些运行时行为作保证。
