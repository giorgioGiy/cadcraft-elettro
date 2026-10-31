# Gestione delle pagine elettriche

Digitare `PAGINE` (anche `GESTIONEPAGINE`), usare il pulsante Gestione pagine
accanto alle frecce dei fogli oppure Visualizza > Fogli > Gestione pagine.
Le pagine sono i layer numerici `1`, `2`, `3` ecc. dello spazio modello.

La finestra mostra numero, due descrizioni ricavate dagli attributi del
cartiglio e l'anteprima della pagina evidenziata. L'anteprima non cambia il
foglio attivo. Apri pagina o un doppio clic rende corrente e visibile il foglio.

## Selezione

- Clic su una riga: selezione singola.
- Caselle o Ctrl+clic: aggiunta/rimozione di singole pagine.
- Maiusc+clic: intervallo di righe.
- Seleziona tutte e Nessuna: gestione dell'intero elenco.

## Operazioni

- **Inserisci:** scegliere posizione e pagina modello, quindi i soli blocchi
  della cornice e del cartiglio da copiare. Gli altri oggetti della pagina
  modello non vengono copiati. Le pagine successive vengono rinumerate.
- **Elimina:** mostra le pagine coinvolte e richiede Conferma eliminazione.
  Elimina i loro oggetti e i layer. I numeri rimasti non vengono compattati;
  usare Rinumera per renderli consecutivi. Se un layer è usato anche nelle
  definizioni dei blocchi o negli oggetti dei layout, l'operazione viene rifiutata.
- **Sposta:** inserisce le pagine selezionate, nel loro ordine attuale, alla
  posizione finale indicata. Tutto l'elenco diventa consecutivo da 1. La
  geometria non viene traslata.
- **Rinumera:** assegna numeri consecutivi alle sole pagine selezionate,
  partendo dal numero indicato. I numeri già usati dalle altre pagine causano
  un errore: nessuna pagina viene sovrascritta.
- **Dati cartiglio:** scegliere il blocco cartiglio (predefinito `att_tav`),
  scegliere un attributo e il suo valore, poi Imposta. Ripetere per gli altri
  campi e premere Salva dati cartiglio. Solo i campi impostati vengono applicati
  alle pagine selezionate; un valore vuoto cancella il testo. Gli attributi
  costanti restano invariati. Se il cartiglio manca su una pagina selezionata,
  l'intera operazione viene annullata.

Le operazioni di struttura aggiornano `NUMFOG` e `NUMFOGSEG` del blocco
cartiglio scelto. L'ultimo foglio ha `NUMFOGSEG` vuoto. I layer comuni restano
separati dalle pagine e conservano la loro visibilità. Il gestore non riordina
né crea i layout carta; lavora sulle pagine sovrapposte nello spazio modello.

Ogni operazione confermata è un solo passo Undo e modifica prima una copia
del disegno: errori di validazione non lasciano modifiche parziali. I layer
bloccati sono protetti. Salvare poi il disegno per aggiornare il file su disco.

## Stampa

Il cartiglio viene riconosciuto automaticamente per pagina: `att_tav`,
`0UNIA3` e `00A3SCHEMI`. In **Blocco cartiglio** lasciare **Automatico** per
gestire anche disegni che usano blocchi diversi fra le pagine. I campi
`OGGETTO1/2`, `SHEET`, `NSHEET`, `SHEETS` corrispondono a descrizione riga 1/2,
numero pagina, pagina successiva e totale fogli. Nei cartigli combinati i
campi delle colonne degli apparecchi non compaiono in **Dati cartiglio**.
Rinumerazione, spostamento e inserimento aggiornano i campi del formato originale.

**Stampa…** stampa le pagine selezionate in PDF. Scegli il formato carta
(da A4 ad A0), l'orientamento e premi **Stampa PDF…** per scegliere il file.
Ogni pagina viene adattata e centrata sul foglio, includendo i layer comuni
visibili e stampabili. Tutte le pagine selezionate sono raccolte in **un unico
PDF multifoglio**, con il nome scelto, in ordine numerico. Le etichette delle
pagine PDF corrispondono ai numeri dei fogli dello schema.
Il bianco puro del layer `CARTIGLIO_ATTRIBUTI` viene stampato in nero per
rendere leggibili i dati sul foglio bianco; il colore nel disegno resta invariato.
Il disegno e la pagina attiva non vengono modificati. Massimo 256 pagine per
operazione. Per la stampante fisica, apri i PDF prodotti e usa la loro stampa.

## Automazione

I comandi JSON non aprono finestre: `sheet.list`, `sheet.show`, `sheet.insert`,
`sheet.delete`, `sheet.move`, `sheet.renumber`, `sheet.attributes`, `sheet.plot`. Il catalogo
dei comandi documenta i parametri; le stesse operazioni sono disponibili da
CLI, script e controllo remoto.

## Revisioni

- 2026-10-24: cartigli automatici per entrambi i formati e campi delle colonne separati.
- 2026-10-24: corretta la leggibilità degli attributi bianchi del cartiglio in stampa.
- 2026-10-24: pulsante Stampa con selezione multipla, carta e orientamento,
  un unico PDF multifoglio senza modificare lo stato del disegno.
- 2026-10-24: finestra più compatta e ridimensionabile, anteprima ingrandita
  proporzionalmente e comandi di selezione e chiusura con ritorno a capo.
- 2026-10-23: gestore pagine con selezione multipla, anteprima, operazioni
  transazionali, cartigli e test di conservazione attraverso DXF.
